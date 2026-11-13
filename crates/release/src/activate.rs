//! Activation (release spec, "Activation"): a verified staged file put in
//! place of its target atomically, with its mode, the directory synced.
//! And the ELF check (release spec, "ELF check") a staged binary passes
//! first, with object's ELF reader
//! (<https://docs.rs/object/0.41.0/object/read/elf/index.html>).
//! sb-go's `Activate` (release/asset.go) is the reference for the steps
//! and the texts. Why not `self-replace`: it copies once more and doesn't
//! sync the directory; tempfile's `persist` is the rename itself.

use std::fs::{File, Permissions};
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use object::elf::{self, FileHeader64};
use object::read::elf::FileHeader as _;
use object::{Endianness, FileKind};
use tempfile::TempPath;

use crate::{Arch, Error};

/// Put `staged` in place of `target` with `mode`: chmod, a rename over
/// `target` (tempfile's `persist`,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.TempPath.html#method.persist>),
/// then an fsync of the directory so the rename survives a crash. `staged`
/// must be in `target`'s directory (else an error before anything
/// changes): a [`Staged`](crate::Staged) download, or a file the caller
/// staged itself (`NamedTempFile::into_temp_path`), the directory
/// compared as written (another spelling of it is refused, safely), and
/// writable by root only. If anything fails before the rename, `staged` is
/// removed and `target` is as it was; a `sync release asset directory:`
/// error comes after it, with the new file already in place.
///
/// # Errors
///
/// [`Error::Activate`] with sb-go's texts (the directory's open and sync
/// in one).
pub fn activate(staged: impl Into<TempPath>, target: &Path, mode: u32) -> Result<(), Error> {
    let staged = staged.into();
    let dir = directory(target);
    if directory(&staged) != dir {
        return Err(Error::Activate(
            "staged release asset must be in the target directory".to_owned(),
        ));
    }
    std::fs::set_permissions(&staged, Permissions::from_mode(mode))
        .map_err(io_failed("set staged release asset permissions"))?;
    staged
        .persist(target)
        .map_err(|err| io_failed("activate release asset")(err.error))?;
    // A directory opened read-only can be synced on Linux
    // (<https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all>).
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(io_failed("sync release asset directory"))
}

/// `path`'s directory, `.` for a bare file name.
fn directory(path: &Path) -> &Path {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// An I/O failure while activating, after `what`.
fn io_failed(what: &'static str) -> impl FnOnce(std::io::Error) -> Error {
    move |err| Error::Activate(format!("{what}: {err}"))
}

/// Why a file isn't a binary for an architecture ([`check_elf`]). Each
/// caller words it with its own sb-go text (release spec, "ELF check").
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ElfError {
    /// Not an ELF executable: unreadable, too short, another format, or
    /// an ELF that is neither an executable nor a shared object (a
    /// relocatable object, a core dump). The cause is object's or the
    /// file system's.
    #[error("not a valid ELF binary: {0}")]
    NotElf(String),
    /// An ELF, but not a 64-bit little-endian one for this architecture
    /// (a 32-bit or big-endian one, another machine): the architecture
    /// asked for.
    #[error("not a 64-bit little-endian {} ELF binary", machine_name(*.0))]
    WrongMachine(Arch),
}

/// Whether `path` is an ELF executable that runs on `arch`: 64-bit,
/// little-endian, its machine `arch`'s, an executable or a shared object
/// (a position-independent executable). Only the header is read.
///
/// # Errors
///
/// [`ElfError`] saying why not.
pub fn check_elf(path: &Path, arch: Arch) -> Result<(), ElfError> {
    let mut header = Vec::new();
    File::open(path)
        .and_then(|file| file.take(ELF_HEADER).read_to_end(&mut header))
        .map_err(|err| ElfError::NotElf(err.to_string()))?;
    let header = header.as_slice();
    let not_elf = |err: object::Error| ElfError::NotElf(err.to_string());
    // The kind first: a 32-bit ELF is an ELF, for another machine.
    // (<https://docs.rs/object/0.41.0/object/read/enum.FileKind.html#method.parse>)
    if FileKind::parse(header).map_err(not_elf)? == FileKind::Elf32 {
        return Err(ElfError::WrongMachine(arch));
    }
    // The header's ident checked, its fields read in its own byte order
    // (<https://docs.rs/object/0.41.0/object/read/elf/trait.FileHeader.html#method.parse>).
    let header = FileHeader64::<Endianness>::parse(header).map_err(not_elf)?;
    let endian = header.endian().map_err(not_elf)?;
    if endian != Endianness::Little || header.e_machine(endian) != machine(arch) {
        return Err(ElfError::WrongMachine(arch));
    }
    let kind = header.e_type(endian);
    if kind != elf::ET_EXEC && kind != elf::ET_DYN {
        return Err(ElfError::NotElf(format!(
            "ELF type {} is neither an executable nor a shared object",
            kind.0
        )));
    }
    Ok(())
}

/// The length of a 64-bit ELF header (`Elf64_Ehdr`, elf(5)), object's
/// type for it: all [`check_elf`] reads.
const ELF_HEADER: u64 = size_of::<FileHeader64<Endianness>>() as u64;

/// `arch`'s machine as ELF's documentation names it.
fn machine_name(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "x86-64",
        Arch::Arm64 => "AArch64",
    }
}

/// `arch`'s `e_machine`.
fn machine(arch: Arch) -> elf::Machine {
    match arch {
        Arch::Amd64 => elf::EM_X86_64,
        Arch::Arm64 => elf::EM_AARCH64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stand-in sb's ELF (this build's architecture), changed by
    /// `edit`, written to a file in `dir`.
    fn elf(dir: &Path, edit: impl FnOnce(&mut Vec<u8>)) -> std::path::PathBuf {
        let mut bytes = crate::testing::stand_in("1.0.0");
        edit(&mut bytes);
        let path = dir.join("binary");
        std::fs::write(&path, bytes).expect("write");
        path
    }

    fn other(arch: Arch) -> Arch {
        match arch {
            Arch::Amd64 => Arch::Arm64,
            Arch::Arm64 => Arch::Amd64,
        }
    }

    /// `e_machine`, little-endian, at its offset in the ELF64 header.
    fn set_machine(bytes: &mut [u8], arch: Arch) {
        let machine: u16 = match arch {
            Arch::Amd64 => 62,
            Arch::Arm64 => 183,
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    }

    #[test]
    fn this_architecture_s_binary_passes() {
        let dir = tempfile::tempdir().expect("dir");
        let arch = Arch::current().expect("arch");
        assert_eq!(check_elf(&elf(dir.path(), |_| {}), arch), Ok(()));
        // The stand-in is position independent (ET_DYN); a fixed-address
        // executable (ET_EXEC, as Go builds sb-go's releases) passes too.
        let exec = elf(dir.path(), |bytes| {
            bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        });
        assert_eq!(check_elf(&exec, arch), Ok(()));
        // The other architecture's machine, checked for that one.
        let path = elf(dir.path(), |bytes| set_machine(bytes, other(arch)));
        assert_eq!(check_elf(&path, other(arch)), Ok(()));
    }

    #[test]
    fn another_architecture_is_the_wrong_machine() {
        let dir = tempfile::tempdir().expect("dir");
        let arch = Arch::current().expect("arch");
        assert_eq!(
            check_elf(&elf(dir.path(), |_| {}), other(arch)),
            Err(ElfError::WrongMachine(other(arch)))
        );
        let path = elf(dir.path(), |bytes| set_machine(bytes, other(arch)));
        assert_eq!(check_elf(&path, arch), Err(ElfError::WrongMachine(arch)));
    }

    /// A 32-bit ELF (class 1) or a big-endian one (data 2, its machine
    /// written big-endian too, as an `aarch64_be` binary's is) isn't this
    /// architecture's either.
    #[test]
    fn a_32_bit_or_big_endian_elf_is_the_wrong_machine() {
        let dir = tempfile::tempdir().expect("dir");
        let arch = Arch::current().expect("arch");
        let class32 = elf(dir.path(), |bytes| bytes[4] = 1);
        assert_eq!(check_elf(&class32, arch), Err(ElfError::WrongMachine(arch)));
        let big = elf(dir.path(), |bytes| {
            bytes[5] = 2;
            let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
            bytes[18..20].copy_from_slice(&machine.to_be_bytes());
        });
        assert_eq!(check_elf(&big, arch), Err(ElfError::WrongMachine(arch)));
    }

    #[test]
    fn a_relocatable_object_or_a_core_dump_is_not_an_executable() {
        let dir = tempfile::tempdir().expect("dir");
        let arch = Arch::current().expect("arch");
        for kind in [1u16, 4] {
            let path = elf(dir.path(), |bytes| {
                bytes[16..18].copy_from_slice(&kind.to_le_bytes());
            });
            assert_eq!(
                check_elf(&path, arch),
                Err(ElfError::NotElf(format!(
                    "ELF type {kind} is neither an executable nor a shared object"
                )))
            );
        }
    }

    #[test]
    fn a_truncated_elf_is_not_an_elf() {
        let dir = tempfile::tempdir().expect("dir");
        let arch = Arch::current().expect("arch");
        let path = elf(dir.path(), |bytes| bytes.truncate(40));
        assert_eq!(
            check_elf(&path, arch),
            Err(ElfError::NotElf(
                "Invalid ELF header size or alignment".to_owned()
            ))
        );
    }

    #[test]
    fn a_script_an_empty_file_or_no_file_is_not_an_elf() {
        let dir = tempfile::tempdir().expect("dir");
        let arch = Arch::current().expect("arch");
        let script = dir.path().join("script");
        std::fs::write(&script, "#!/bin/sh\necho 'not an ELF binary'\n").expect("script");
        assert_eq!(
            check_elf(&script, arch),
            Err(ElfError::NotElf("Unknown file magic".to_owned()))
        );
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "").expect("empty");
        assert_eq!(
            check_elf(&empty, arch),
            Err(ElfError::NotElf("Could not read file magic".to_owned()))
        );
        let Err(ElfError::NotElf(cause)) = check_elf(&dir.path().join("gone"), arch) else {
            panic!("a missing file isn't an ELF");
        };
        assert!(cause.contains("No such file"), "{cause}");
    }

    #[test]
    fn the_errors_say_why() {
        assert_eq!(
            ElfError::NotElf("Unknown file magic".to_owned()).to_string(),
            "not a valid ELF binary: Unknown file magic"
        );
        assert_eq!(
            ElfError::WrongMachine(Arch::Arm64).to_string(),
            "not a 64-bit little-endian AArch64 ELF binary"
        );
        assert_eq!(
            ElfError::WrongMachine(Arch::Amd64).to_string(),
            "not a 64-bit little-endian x86-64 ELF binary"
        );
    }
}
