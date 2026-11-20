//! GPU (motd spec, Sections and "GPU names"; decision 2): the display
//! devices from sysfs, named from the host's `/usr/share/misc/pci.ids`
//! as lspci names them, where sb-go parsed `lspci`'s output
//! (`motd/info.go:776-846`).
//!
//! - The devices are `/sys/bus/pci/devices/*` whose `class` is VGA
//!   (0300), 3D (0302) or other display (0380) controller, the three
//!   lspci class names sb-go looked for; their IDs come from `vendor`,
//!   `device` and `revision`, the attributes the kernel documents
//!   (<https://www.kernel.org/doc/Documentation/ABI/testing/sysfs-bus-pci>).
//!   Why not pci-info 0.4.0: it reads /proc/bus/pci's config headers, not
//!   sysfs, its bus parse takes no domain past four hex digits (Intel
//!   VMD's `10000:`), and it names nothing
//!   (<https://docs.rs/crate/pci-info/0.4.0/source/src/enumerators/linux/proc_fs.rs>).
//! - The names follow pciutils v3.7.0
//!   (<https://github.com/pciutils/pciutils/blob/v3.7.0/lib/names.c>,
//!   `format_name_pair`, and `lspci.c`'s `show_terse` for ` (rev xx)` and
//!   its address order).
//! - The parser is sb's own, as domain logic: the pci-ids crate embeds
//!   its own snapshot (+3.4 MB) and `pciids` 0.1.4 has had no release
//!   since 2020 (motd spec, Tech Stack, "Considered and not used").
//!   pciid-parser 0.8.1 does read the host's file, but its `parse_db`
//!   ends at the first line it can't parse and takes invalid UTF-8 as an
//!   I/O error, and its event parser is private, so it can't skip and
//!   count a bad line or replace a bad byte as the spec asks
//!   (<https://docs.rs/crate/pciid-parser/0.8.1/source/src/parser.rs>,
//!   `next_event`; <https://docs.rs/crate/pciid-parser/0.8.1/source/src/lib.rs>,
//!   `mod parser`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::text::one_line;

/// The most of pci.ids read (motd spec: the file is about 1.4 MB).
const IDS_CAP: u64 = 8 << 20;

/// The display classes, as `class >> 8` (class and subclass).
const DISPLAY: [u32; 3] = [0x0300, 0x0302, 0x0380];

/// sb-go's BMC and server-management adapters, never shown
/// (`motd/info.go:780-790`), matched in its way: anywhere in the shown
/// name, case ignored.
const EXCLUDED: [&str; 9] = [
    "ASPEED",
    "Matrox MGA",
    "Cirrus Logic",
    "XGI",
    "Silicon Motion",
    "Hisilicon",
    "ServerEngines",
    "Nuvoton",
    "Pilot",
];

/// Where GPU reads; the tests point it at a temporary directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sources {
    /// sysfs's PCI devices, one directory per address.
    pub devices: PathBuf,
    /// The PCI ID database lspci reads.
    pub ids: PathBuf,
    /// The most of it read.
    pub cap: u64,
}

impl Sources {
    /// This host's: sysfs and Ubuntu's `pci.ids` package.
    #[must_use]
    pub fn system() -> Self {
        Self {
            devices: PathBuf::from("/sys/bus/pci/devices"),
            ids: PathBuf::from("/usr/share/misc/pci.ids"),
            cap: IDS_CAP,
        }
    }
}

/// One display device, as lspci lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Adapter {
    address: String,
    vendor: u16,
    device: u16,
    revision: u8,
}

/// The names pci.ids gives the wanted vendors and their devices, and how
/// many lines it had that pciutils' format doesn't allow.
#[derive(Debug, Default, PartialEq, Eq)]
struct Names<'a> {
    vendors: HashMap<u16, Cow<'a, str>>,
    devices: HashMap<(u16, u16), Cow<'a, str>>,
    malformed: usize,
}

/// GPU on this host.
pub async fn gpu_here() -> String {
    gpu_at(Sources::system()).await
}

/// GPU on this host, and its `--json` value `{devices: [{name}]}`, one
/// per line (motd spec, `--json`); none is `empty`.
pub async fn gpu_here_out() -> crate::Out {
    gpu_out(gpu_here().await)
}

/// The value of GPU's `text`: each line a device's name.
fn gpu_out(text: String) -> crate::Out {
    let devices: Vec<serde_json::Value> = text
        .lines()
        .map(|name| serde_json::json!({ "name": name }))
        .collect();
    crate::Out::value(text, serde_json::json!({ "devices": devices }))
}

/// GPU from `sources`, read on tokio's blocking pool: pci.ids is a large
/// file, and the runtime is current-thread
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
/// A panic in the read reaches the runner, which shows its error line.
pub async fn gpu_at(sources: Sources) -> String {
    // The run's span goes along (cli-core's `spawn`).
    match sb_cli_core::spawn::blocking(move || gpu(&sources)).await {
        Ok(value) => value,
        Err(join) => match join.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(_) => String::new(),
        },
    }
}

/// GPU: one line per display device that isn't a BMC's, in address
/// order; empty, so the line is hidden, when there is none (sb-go's
/// rule). pci.ids is read only when there is a device to name; without
/// it every device reads in pciutils' unknown-ID form, and the run log
/// says why.
fn gpu(sources: &Sources) -> String {
    let found = devices(&sources.devices);
    if found.is_empty() {
        return String::new();
    }
    let text = match read_ids(&sources.ids, sources.cap) {
        Ok(text) => Some(text),
        Err(err) => {
            tracing::warn!(file = %sources.ids.display(), "pci.ids not read: {err}");
            None
        }
    };
    let wanted: Vec<u16> = found.iter().map(|device| device.vendor).collect();
    let names = text.as_deref().map_or_default(|text| parse(text, &wanted));
    if names.malformed > 0 {
        tracing::warn!(
            file = %sources.ids.display(),
            malformed = names.malformed,
            "pci.ids lines skipped"
        );
    }
    found
        .iter()
        .map(|device| one_line(name(device, &names).as_bytes()))
        .filter(|shown| !excluded(shown))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether sb-go leaves `shown` out (Go's `strings.ToUpper` on both).
fn excluded(shown: &str) -> bool {
    let upper = shown.to_uppercase();
    EXCLUDED
        .iter()
        .any(|bmc| upper.contains(&bmc.to_uppercase()))
}

/// The display devices under `dir`, in lspci's order (domain, bus,
/// device, function); none when sysfs can't be listed. A device whose
/// IDs can't be read is left out.
fn devices(dir: &Path) -> Vec<Adapter> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), "PCI devices not listed: {err}");
            return Vec::new();
        }
    };
    let mut found: Vec<Adapter> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let class = hex_attribute(&path.join("class"))?;
            if !DISPLAY.contains(&(class >> 8)) {
                return None;
            }
            Some(Adapter {
                address: entry.file_name().to_string_lossy().into_owned(),
                vendor: u16::try_from(hex_attribute(&path.join("vendor"))?).ok()?,
                device: u16::try_from(hex_attribute(&path.join("device"))?).ok()?,
                revision: hex_attribute(&path.join("revision"))
                    .and_then(|revision| u8::try_from(revision).ok())
                    .unwrap_or(0),
            })
        })
        .collect();
    found.sort_by(|a, b| {
        let (ka, kb) = (address_key(&a.address), address_key(&b.address));
        (ka.is_none(), ka, &a.address).cmp(&(kb.is_none(), kb, &b.address))
    });
    found
}

/// A sysfs attribute written as `0x` and hex digits, as the kernel
/// writes `class`, `vendor`, `device` and `revision`.
fn hex_attribute(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let digits = text.trim().strip_prefix("0x")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// A PCI address `dddd:bb:dd.f` as numbers, for lspci's order; none for
/// a name in another form, which then sorts last.
fn address_key(address: &str) -> Option<(u32, u8, u8, u8)> {
    let (domain, rest) = address.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (slot, function) = rest.split_once('.')?;
    Some((
        u32::from_str_radix(domain, 16).ok()?,
        u8::from_str_radix(bus, 16).ok()?,
        u8::from_str_radix(slot, 16).ok()?,
        u8::from_str_radix(function, 16).ok()?,
    ))
}

/// pci.ids whole, as bytes (the parser reads it as UTF-8 with invalid
/// bytes replaced, only where it keeps a name); an error when it can't be
/// read or is over `cap` bytes.
fn read_ids(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
        return Err(std::io::Error::other(format!("over {cap} bytes")));
    }
    Ok(bytes)
}

/// Where the parse stands: before any vendor, under a vendor line that
/// isn't one (its device lines are skipped with it), or under a vendor.
#[derive(Clone, Copy)]
enum Under {
    Start,
    Malformed,
    Vendor(u16),
}

/// The vendor lines (`vvvv  name`) of `wanted` and their device lines (a
/// tab, then `dddd  name`), as pciutils' format has them. Comments,
/// blank lines and subsystem lines (two tabs) are skipped, and so is
/// everything from the class section (the first `C ` line) on. Any other
/// line is malformed: skipped and counted. A repeated ID keeps its first
/// name. The text is read as UTF-8 with invalid bytes replaced (Unicode
/// whitespace trimmed, as `str::trim`), the same as reading the whole
/// file so: every byte the format's structure is made of is ASCII, which
/// a replacement never touches.
///
/// Why bytes, line ends found with memchr
/// (<https://docs.rs/memchr/2.8.3/memchr/struct.Memchr.html>): of the
/// file's 35,000 lines only the wanted vendors' few hundred are kept, so
/// the rest are only told apart by their first bytes (well-formed or
/// not), without checking the 1.3 MB as UTF-8 or trimming each line.
fn parse<'a>(text: &'a [u8], wanted: &[u16]) -> Names<'a> {
    let mut names = Names::default();
    let mut under = Under::Start;
    let mut start = 0;
    let ends = memchr::memchr_iter(b'\n', text).chain(std::iter::once(text.len()));
    for end in ends {
        let line = &text[start..end];
        start = end + 1;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(b"C ") {
            break;
        }
        let tabs = line.iter().take_while(|&&b| b == b'\t').count();
        let rest = &line[tabs..];
        if tabs > 1 || rest.first() == Some(&b'#') {
            continue;
        }
        let parsed = match id_line(rest) {
            Line::Blank => continue,
            Line::Malformed => None,
            Line::Id(id, tail) => Some((id, tail)),
        };
        match (tabs, parsed) {
            (0, Some((vendor, tail))) => {
                under = Under::Vendor(vendor);
                if wanted.contains(&vendor) {
                    names.vendors.entry(vendor).or_insert_with(|| trimmed(tail));
                }
            }
            (0, None) => {
                under = Under::Malformed;
                names.malformed += 1;
            }
            (_, parsed) => match (under, parsed) {
                (Under::Vendor(vendor), Some((device, tail))) => {
                    if wanted.contains(&vendor) {
                        names
                            .devices
                            .entry((vendor, device))
                            .or_insert_with(|| trimmed(tail));
                    }
                }
                (Under::Malformed, _) => {}
                _ => names.malformed += 1,
            },
        }
    }
    names
}

/// A vendor or device line past its tab: an ID and the rest of the line,
/// blank (only whitespace), or anything else.
enum Line<'a> {
    Id(u16, &'a [u8]),
    Blank,
    Malformed,
}

/// `rest` as pciutils reads an ID line (`id_hex` and `id_white_p`), from
/// its bytes: four hex digits, a space or tab, then a name that isn't
/// only whitespace. The name's
/// first byte decides in nearly every line; a line whose first byte past
/// the spaces isn't ASCII is read as text to decide.
fn id_line(rest: &[u8]) -> Line<'_> {
    let id = rest
        .get(..5)
        .filter(|head| {
            head[..4].iter().all(u8::is_ascii_hexdigit) && matches!(head[4], b' ' | b'\t')
        })
        .and_then(|head| std::str::from_utf8(&head[..4]).ok())
        .and_then(|digits| u16::from_str_radix(digits, 16).ok());
    if let Some(id) = id {
        let tail = &rest[4..];
        let named = match tail.iter().find(|&&b| !is_space(b)) {
            Some(b) if b.is_ascii() => true,
            Some(_) => !trimmed(tail).is_empty(),
            None => false,
        };
        if named {
            return Line::Id(id, tail);
        }
    }
    if rest.iter().all(|&b| is_space(b)) || (!rest.is_ascii() && trimmed(rest).is_empty()) {
        Line::Blank
    } else {
        Line::Malformed
    }
}

/// The ASCII characters `char::is_whitespace` takes (vertical tab too,
/// which `u8::is_ascii_whitespace` doesn't).
fn is_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ')
}

/// `bytes` as UTF-8 with invalid bytes replaced, trimmed as `str::trim`
/// trims; borrowed when valid, as Ubuntu's file is.
fn trimmed(bytes: &[u8]) -> Cow<'_, str> {
    match String::from_utf8_lossy(bytes) {
        Cow::Borrowed(text) => Cow::Borrowed(text.trim()),
        Cow::Owned(text) => Cow::Owned(text.trim().to_string()),
    }
}

/// lspci's name for `adapter`: `<vendor> <device>`, or pciutils' forms for
/// an unknown ID (`<vendor> Device dddd`, `Device vvvv:dddd`), then
/// ` (rev xx)` when the revision isn't zero.
fn name(adapter: &Adapter, names: &Names<'_>) -> String {
    let (vendor, id) = (adapter.vendor, adapter.device);
    let shown = match (names.vendors.get(&vendor), names.devices.get(&(vendor, id))) {
        (Some(v), Some(d)) => format!("{v} {d}"),
        (Some(v), None) => format!("{v} Device {id:04x}"),
        (None, _) => format!("Device {vendor:04x}:{id:04x}"),
    };
    match adapter.revision {
        0 => shown,
        revision => format!("{shown} (rev {revision:02x})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthetic pci.ids: a comment, subsystem lines, a malformed
    /// vendor and device line, a non-UTF-8 byte and a class section.
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pci.ids")
    }

    /// A device directory under `dir`, as sysfs writes its attributes.
    fn device(dir: &Path, address: &str, class: &str, ids: (&str, &str), revision: &str) {
        let path = dir.join(address);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("class"), format!("{class}\n")).unwrap();
        std::fs::write(path.join("vendor"), format!("{}\n", ids.0)).unwrap();
        std::fs::write(path.join("device"), format!("{}\n", ids.1)).unwrap();
        std::fs::write(path.join("revision"), format!("{revision}\n")).unwrap();
    }

    fn sources(dir: &Path, ids: PathBuf) -> Sources {
        Sources {
            devices: dir.to_path_buf(),
            ids,
            cap: IDS_CAP,
        }
    }

    #[test]
    fn the_system_sources_are_sysfs_and_ubuntus_pci_ids() {
        let system = Sources::system();
        assert_eq!(system.devices, Path::new("/sys/bus/pci/devices"));
        assert_eq!(system.ids, Path::new("/usr/share/misc/pci.ids"));
        assert_eq!(system.cap, 8 * 1024 * 1024);
    }

    #[test]
    fn names_take_lspcis_forms_known_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        device(d, "0000:01:00.0", "0x030000", ("0x10de", "0x2204"), "0xa1");
        device(d, "0000:02:00.0", "0x030200", ("0x10de", "0x9999"), "0x00");
        device(d, "0000:03:00.0", "0x038000", ("0x1b36", "0x0100"), "0x05");
        // In the file only as a subsystem, and only after the classes.
        device(d, "0000:04:00.0", "0x030000", ("0x10de", "0x12a2"), "0x00");
        device(d, "0000:05:00.0", "0x030000", ("0xeeee", "0x7777"), "0x00");
        // prog-if 01 is still a VGA controller.
        device(d, "0000:06:00.0", "0x030001", ("0x8086", "0x5912"), "0x04");
        assert_eq!(
            gpu(&sources(d, fixture())),
            "NVIDIA Corporation GA102 [GeForce RTX 3090] (rev a1)\n\
             NVIDIA Corporation Device 9999\n\
             Device 1b36:0100 (rev 05)\n\
             NVIDIA Corporation Device 12a2\n\
             Device eeee:7777\n\
             Intel Corporation HD Graphics 630 (rev 04)"
        );
    }

    #[test]
    fn other_classes_are_not_gpus() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        device(d, "0000:00:1f.0", "0x020000", ("0x8086", "0x5912"), "0x00");
        device(d, "0000:00:1f.1", "0x030100", ("0x8086", "0x5912"), "0x00");
        device(d, "0000:00:1f.2", "0x040300", ("0x8086", "0x5912"), "0x00");
        device(d, "0000:00:1f.3", "0x038100", ("0x8086", "0x5912"), "0x00");
        assert_eq!(gpu(&sources(d, fixture())), "");
    }

    /// No display device, or no sysfs: the line is hidden, and pci.ids
    /// isn't needed.
    #[test]
    fn no_gpu_hides_the_line() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(gpu(&sources(dir.path(), dir.path().join("none"))), "");
        let gone = dir.path().join("gone");
        assert_eq!(gpu(&sources(&gone, fixture())), "");
    }

    #[test]
    fn bmc_adapters_are_left_out_as_sb_go_leaves_them() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        device(d, "0000:01:00.0", "0x030000", ("0x1a03", "0x2000"), "0x41");
        device(d, "0000:02:00.0", "0x030000", ("0x10de", "0x1eb8"), "0xa1");
        assert_eq!(
            gpu(&sources(d, fixture())),
            "NVIDIA Corporation TU104GL [Tesla T4] (rev a1)"
        );
        for name in [
            "ASPEED Technology, Inc. ASPEED Graphics Family (rev 41)",
            "Matrox MGA G200",
            "matrox mga g200",
            "Cirrus Logic GD 5446",
            "XGI Technology Inc. (eXtreme Graphics Innovation) Z7/Z9",
            "Silicon Motion, Inc. SM750 (rev a1)",
            "Huawei Technologies Co., Ltd. Hi1710 [iBMC Intelligent Management system chip w/VGA support] Hisilicon",
            "ServerEngines Pilot III",
            "Nuvoton WPCM450",
            "Aspeed pilot",
        ] {
            assert!(excluded(name), "{name}");
        }
        assert!(!excluded("NVIDIA Corporation Device 2204"));
        assert!(!excluded("Device 1a03:2000"));
    }

    /// lspci's order: domain, bus, device, function, as numbers.
    #[test]
    fn devices_come_in_address_order_on_any_domain() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for (address, id) in [
            ("a000:00:00.0", "0x0005"),
            ("0000:65:00.0", "0x0003"),
            // 0x10000 is past 0xa000, though it sorts first as text.
            ("10000:00:00.0", "0x0004"),
            ("0000:01:00.1", "0x0002"),
            ("0000:01:00.0", "0x0001"),
            ("0001:00:02.0", "0x5912"),
            ("odd", "0x0006"),
        ] {
            device(d, address, "0x030000", ("0x8086", id), "0x00");
        }
        assert_eq!(
            gpu(&sources(d, fixture())),
            "Intel Corporation Device 0001\n\
             Intel Corporation Device 0002\n\
             Intel Corporation Device 0003\n\
             Intel Corporation HD Graphics 630\n\
             Intel Corporation Device 0005\n\
             Intel Corporation Device 0004\n\
             Intel Corporation Device 0006"
        );
        assert_eq!(address_key("0001:02:1f.7"), Some((1, 2, 0x1f, 7)));
        assert_eq!(address_key("0000:00:00"), None);
        assert_eq!(address_key("0000:00"), None);
        assert_eq!(address_key("x"), None);
    }

    /// A device whose IDs can't be read is left out; a missing or odd
    /// revision is zero.
    #[test]
    fn unreadable_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        device(d, "0000:01:00.0", "0x030000", ("0x10de", "nope"), "0x00");
        device(d, "0000:02:00.0", "0x030000", ("0x10de", "0x12345"), "0x00");
        device(d, "0000:03:00.0", "0x030000", ("10de", "0x2204"), "0x00");
        device(d, "0000:04:00.0", "0x030000", ("0x", "0x2204"), "0x00");
        // `from_str_radix` alone would take the sign.
        device(d, "0000:04:00.1", "0x030000", ("0x+10de", "0x2204"), "0x00");
        device(d, "0000:05:00.0", "0x030000", ("0x10de", "0x2204"), "0x1ff");
        device(d, "0000:06:00.0", "0x030000", ("0x10de", "0x2204"), "0x01");
        std::fs::remove_file(d.join("0000:06:00.0/revision")).unwrap();
        device(d, "0000:07:00.0", "class", ("0x10de", "0x2204"), "0x01");
        std::fs::write(d.join("stray"), "a file, not a device").unwrap();
        assert_eq!(
            gpu(&sources(d, fixture())),
            "NVIDIA Corporation GA102 [GeForce RTX 3090]\n\
             NVIDIA Corporation GA102 [GeForce RTX 3090]"
        );
        assert_eq!(hex_attribute(&d.join("0000:01:00.0/vendor")), Some(0x10de));
        assert_eq!(hex_attribute(&d.join("0000:04:00.0/vendor")), None);
        assert_eq!(hex_attribute(&d.join("0000:04:00.1/vendor")), None);
        assert_eq!(hex_attribute(&d.join("0000:01:00.0/device")), None);
    }

    /// Without the file (missing, a directory, over the cap) every device
    /// reads `Device vvvv:dddd`, and the cause is an error to log.
    #[test]
    fn without_pci_ids_devices_read_by_number() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("devices");
        std::fs::create_dir(&d).unwrap();
        device(&d, "0000:01:00.0", "0x030000", ("0x10de", "0x2204"), "0xa1");
        let by_number = "Device 10de:2204 (rev a1)";
        let missing = dir.path().join("missing");
        assert_eq!(gpu(&sources(&d, missing.clone())), by_number);
        let err = read_ids(&missing, IDS_CAP).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

        let directory = dir.path().join("dir");
        std::fs::create_dir(&directory).unwrap();
        assert_eq!(gpu(&sources(&d, directory.clone())), by_number);
        assert!(read_ids(&directory, IDS_CAP).is_err());

        let size = std::fs::metadata(fixture()).unwrap().len();
        let capped = Sources {
            cap: size - 1,
            ..sources(&d, fixture())
        };
        assert_eq!(gpu(&capped), by_number);
        let err = read_ids(&fixture(), size - 1).unwrap_err();
        assert_eq!(err.to_string(), format!("over {} bytes", size - 1));
        assert!(read_ids(&fixture(), size).is_ok());
    }

    #[test]
    fn the_parser_keeps_vendors_and_devices_and_counts_malformed_lines() {
        let text = read_ids(&fixture(), IDS_CAP).unwrap();
        let names = parse(&text, &[0x10de, 0x8086, 0x1234, 0xeeee, 0xabcd, 0x1002]);
        assert_eq!(names.vendors[&0x10de], "NVIDIA Corporation");
        assert_eq!(names.devices[&(0x10de, 0x1eb8)], "TU104GL [Tesla T4]");
        assert_eq!(names.devices[&(0x8086, 0x5912)], "HD Graphics 630");
        // A tab after the ID is whitespace as a space is.
        assert_eq!(
            names.devices[&(0x1002, 0x73bf)],
            "Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]"
        );
        // The invalid byte is replaced.
        assert_eq!(names.vendors[&0x1234], "Caf\u{fffd} Vendor");
        assert_eq!(names.devices[&(0x1234, 0x1111)], "Display \u{fffd}");
        // Subsystems, the class section and repeats.
        assert!(!names.devices.contains_key(&(0x10de, 0x12a2)));
        assert!(!names.vendors.contains_key(&0xeeee));
        assert_eq!(names.devices[&(0x10de, 0x2204)], "GA102 [GeForce RTX 3090]");
        // `abcd` with no name, `\tzz12 ...`, `\t1` and a device line
        // before any vendor; the devices under `abcd` go with it.
        assert!(!names.vendors.contains_key(&0xabcd));
        assert_eq!(names.malformed, 4);
        // Only the wanted vendors are kept.
        let few = parse(&text, &[0x8086]);
        assert_eq!(few.vendors.len(), 1);
        assert_eq!(few.devices.len(), 1);
        assert_eq!(few.malformed, 4);
    }

    #[test]
    fn crlf_lines_and_tabs_after_an_id_are_read() {
        let names = parse(b"0001\tA\r\n\t0002  B \r\n", &[1]);
        assert_eq!(names.vendors[&1], "A");
        assert_eq!(names.devices[&(1, 2)], "B");
        assert_eq!(names.malformed, 0);
    }

    /// `id_line`'s answer for `rest`, as text: the ID and trimmed name,
    /// `blank` or `malformed`.
    fn read(rest: &[u8]) -> String {
        match id_line(rest) {
            Line::Id(id, tail) => format!("{id:04x} {}", trimmed(tail)),
            Line::Blank => "blank".into(),
            Line::Malformed => "malformed".into(),
        }
    }

    #[test]
    fn id_lines_need_four_hex_digits_whitespace_and_a_name() {
        assert_eq!(read(b"10de  NVIDIA"), "10de NVIDIA");
        assert_eq!(read(b"10DE NVIDIA"), "10de NVIDIA");
        assert_eq!(read(b"10de\tNVIDIA \t"), "10de NVIDIA");
        assert_eq!(read(b"10de"), "malformed");
        assert_eq!(read(b"10de  "), "malformed");
        assert_eq!(read(b"10de \x0b"), "malformed");
        assert_eq!(read(b"10deX name"), "malformed");
        assert_eq!(read(b"+1de name"), "malformed");
        assert_eq!(read("10dé name".as_bytes()), "malformed");
        assert_eq!(read(b"10d"), "malformed");
        // Unicode whitespace is trimmed as `str::trim` trims it; an
        // invalid byte is a name.
        assert_eq!(read("10de \u{a0}".as_bytes()), "malformed");
        assert_eq!(read("10de \u{3000}Name\u{a0}".as_bytes()), "10de Name");
        assert_eq!(read(b"10de \xff"), "10de \u{fffd}");
        assert_eq!(read(b""), "blank");
        assert_eq!(read(b" \t\x0b"), "blank");
        assert_eq!(read("\u{a0} ".as_bytes()), "blank");
        assert_eq!(read(b"\xff"), "malformed");
    }

    /// pciutils' reading of an ID line, on text: the parser before it
    /// read bytes, kept as the reference the byte parser must match.
    fn id_and_name(rest: &str) -> Option<(u16, &str)> {
        let (id, tail) = rest.split_at_checked(4)?;
        if !id.bytes().all(|b| b.is_ascii_hexdigit()) || !tail.starts_with([' ', '\t']) {
            return None;
        }
        let name = tail.trim();
        if name.is_empty() {
            return None;
        }
        Some((u16::from_str_radix(id, 16).ok()?, name))
    }

    /// The reference's ID lines: four hex digits, whitespace and a name.
    #[test]
    fn the_references_id_lines_need_four_hex_digits_whitespace_and_a_name() {
        assert_eq!(id_and_name("10de  NVIDIA"), Some((0x10de, "NVIDIA")));
        assert_eq!(id_and_name("10DE NVIDIA"), Some((0x10de, "NVIDIA")));
        assert_eq!(id_and_name("10de"), None);
        assert_eq!(id_and_name("10de  "), None);
        assert_eq!(id_and_name("10deX name"), None);
        assert_eq!(id_and_name("+1de name"), None);
        assert_eq!(id_and_name("10dé name"), None);
        assert_eq!(id_and_name("10d"), None);
    }

    /// The reference parse, on the whole file read as UTF-8 with invalid
    /// bytes replaced (the parser before it read bytes).
    fn reference(bytes: &[u8], wanted: &[u16]) -> Names<'static> {
        let text = String::from_utf8_lossy(bytes);
        let mut names = Names::default();
        let mut under = Under::Start;
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.starts_with("C ") {
                break;
            }
            let rest = line.trim_start_matches('\t');
            if rest.trim().is_empty() || rest.starts_with('#') {
                continue;
            }
            let owned = |name: &str| Cow::Owned(name.to_string());
            match (line.len() - rest.len(), id_and_name(rest)) {
                (0, Some((vendor, name))) => {
                    under = Under::Vendor(vendor);
                    if wanted.contains(&vendor) {
                        names.vendors.entry(vendor).or_insert_with(|| owned(name));
                    }
                }
                (0, None) => {
                    under = Under::Malformed;
                    names.malformed += 1;
                }
                (1, parsed) => match (under, parsed) {
                    (Under::Vendor(vendor), Some((device, name))) => {
                        if wanted.contains(&vendor) {
                            names
                                .devices
                                .entry((vendor, device))
                                .or_insert_with(|| owned(name));
                        }
                    }
                    (Under::Malformed, _) => {}
                    _ => names.malformed += 1,
                },
                _ => {}
            }
        }
        names
    }

    /// The byte parser gives the reference's names and count on the
    /// fixture, on lines made of the format's edge cases in every order
    /// of a few, and on the host's own pci.ids where it has one (read
    /// only).
    #[test]
    fn the_byte_parser_matches_the_text_parser() {
        let wanted = [0x10de, 0x8086, 0x1234, 0xeeee, 0xabcd, 0x1002, 0x0001];
        let fixture = read_ids(&fixture(), IDS_CAP).unwrap();
        assert_eq!(parse(&fixture, &wanted), reference(&fixture, &wanted));
        let pieces: [&[u8]; 16] = [
            b"0001  A\n",
            b"\t0002  B\n",
            b"\t\t0003 0004  C\n",
            b"0001\n",
            b"\tzz02 x\n",
            b"# c\n",
            b" \n",
            "\u{a0}\n".as_bytes(),
            b"\t\xff\n",
            b"\t0002  \xe2\x82\n",
            b"0001 \xff \r\n",
            "\t0005 \u{3000}\n".as_bytes(),
            b"\r\n",
            b"\t#0002 x\n",
            b"C 03  Display\n",
            b"\t0006",
        ];
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    let text = [a, b, c, b"\t0007  D\n"].concat();
                    assert_eq!(
                        parse(&text, &wanted),
                        reference(&text, &wanted),
                        "{:?}",
                        String::from_utf8_lossy(&text)
                    );
                }
            }
        }
        if let Ok(host) = read_ids(Path::new("/usr/share/misc/pci.ids"), IDS_CAP) {
            for wanted in [&[0x10de, 0x8086, 0x1002][..], &[0x1a03], &[0x15ad, 0x1234]] {
                let names = parse(&host, wanted);
                assert_eq!(names, reference(&host, wanted));
            }
        }
    }

    /// A name from the file is made safe to draw.
    #[test]
    fn names_go_through_display_text() {
        let dir = tempfile::tempdir().unwrap();
        let ids = dir.path().join("pci.ids");
        std::fs::write(
            &ids,
            "10de  Evil\x1b]0;title\x07 Corp\n\t2204  GPU\x1b[2J\n",
        )
        .unwrap();
        let devices = dir.path().join("devices");
        std::fs::create_dir(&devices).unwrap();
        device(
            &devices,
            "0000:01:00.0",
            "0x030000",
            ("0x10de", "0x2204"),
            "0x00",
        );
        assert_eq!(gpu(&sources(&devices, ids)), "Evil Corp GPU");
    }

    #[tokio::test]
    async fn the_gpu_line_is_read_off_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        device(
            dir.path(),
            "0000:01:00.0",
            "0x030000",
            ("0x10de", "0x2204"),
            "0xa1",
        );
        assert_eq!(
            gpu_at(sources(dir.path(), fixture())).await,
            "NVIDIA Corporation GA102 [GeForce RTX 3090] (rev a1)"
        );
        // This host's line is the same read as the blocking one.
        assert_eq!(gpu_here().await, gpu(&Sources::system()));
    }

    /// GPU's `--json` value (motd spec, `--json`): a device per line;
    /// none is `empty`.
    #[test]
    fn the_value_lists_each_device() {
        assert_eq!(
            gpu_out("NVIDIA GA102\nIntel UHD 630".to_string()).json,
            crate::Json::Value(serde_json::json!({
                "devices": [{"name": "NVIDIA GA102"}, {"name": "Intel UHD 630"}]
            }))
        );
        assert_eq!(gpu_out(String::new()).json, crate::Json::Empty);
    }
}
