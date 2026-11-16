#!/usr/bin/env bash
{
# The whole script is one brace group, read by bash to its closing brace
# before any of it runs: a download cut short under `curl ... | bash` is a
# syntax error (non-zero), never a partial run that exits 0.
#
# Saltbox installer: downloads, verifies and installs sb, then runs `sb
# setup` (ops spec, "The install script"). It starts from sb-go's
# install.sh, plus `-b <branch>` from the served sb_install.sh:
#
#   curl -sL https://install.saltbox.dev | sudo -H bash && cd /srv/git/saltbox
#   curl -sL https://install.saltbox.dev | sudo -H bash -s -- -b develop -v
#
# (or `wget -qO-` for `curl -sL`). Nothing here reads stdin, which under
# `curl ... | bash` is the rest of this script; `sb setup` runs attached
# to the terminal, its stdin /dev/null; any failure exits non-zero, so
# `&& cd /srv/git/saltbox` runs only after a complete install.
#
# Copyright the Saltbox contributors; GPL-3.0-only, as sb-go's install.sh.

set -euo pipefail

# The repository and asset sb's releases come from: sb-go's, the only
# channel there is, until release preparation names sb-rust's (decided.md
# D-Q3; sb's self-update uses the same, crates/sb/src/self_update.rs).
readonly GITHUB_REPO="saltyorg/sb-go"
readonly DOWNLOAD_BINARY_NAME="sb_linux_amd64"
readonly BINARY_NAME="sb"
readonly MIN_BINARY_SIZE=1000000 # 1 MB: a smaller download is an error page
readonly FRESH_INSTALL_UBUNTU_VERSIONS="24.04, 26.04"
readonly RUNTIME_UBUNTU_VERSIONS="22.04, 24.04, 26.04"
readonly DOWNLOAD_ATTEMPTS=3
# A tag is a version (`v1.2.3`, `1.2.3-rc.1`): it goes into a URL and a
# comparison, so nothing else is taken from the release metadata.
readonly TAG_PATTERN='^v?[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$'
# Fixed here, never read from the environment; the policy test, sourcing
# the script, sets them on its own world. STAGED is the binary while it is
# being put in place, removed on exit if it is still there.
INSTALL_PATH="/usr/local/bin/${BINARY_NAME}"
SALTBOX_ROOT="/srv/git/saltbox"
OS_RELEASE_FILE="/etc/os-release"
RETRY_DELAY=2
DOWNLOAD_TOOL=""       # set by check_dependencies
FORCE_DOWNLOAD_TOOL="" # --force-curl / --force-wget
REPAIR_MODE=false
VERBOSE_MODE=false
BRANCH="master"
TEMP_DIR=""
STAGED=""
# Network limits: a stalled server ends the try instead of hanging it.
readonly CONNECT_TIMEOUT=10
readonly METADATA_TIMEOUT=30
readonly STALL_SECONDS=60

readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly NC='\033[0m'

# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() {
  if [[ -n ${TEMP_DIR} && -d ${TEMP_DIR} ]]; then
    rm -rf -- "${TEMP_DIR}"
  fi
  if [[ -n ${STAGED} ]]; then
    rm -f -- "${STAGED}"
  fi
}
trap cleanup EXIT

# The colour codes expanded, the message as given: text from a download
# or a program's output never has its escapes turned into codes.
log_info() { printf '%b[INFO]%b %s\n' "${BLUE}" "${NC}" "$*" >&2; }
log_success() { printf '%b[SUCCESS]%b %s\n' "${GREEN}" "${NC}" "$*" >&2; }
log_warn() { printf '%b[WARN]%b %s\n' "${YELLOW}" "${NC}" "$*" >&2; }
log_error() { printf '%b[ERROR]%b %s\n' "${RED}" "${NC}" "$*" >&2; }
# The installer's own detail lines, shown with -v (sb setup has no
# verbose mode, so -v stays here).
log_detail() {
  if [[ ${VERBOSE_MODE} == true ]]; then
    log_info "$@"
  fi
}

check_privileges() {
  if [[ ${EUID} -ne 0 ]]; then
    log_error "This script must be run as root or with sudo"
    exit 1
  fi
}

# Anything at the Saltbox root, a symlink included, as sb setup's guard.
check_saltbox_exists() {
  if [[ -e ${SALTBOX_ROOT} || -L ${SALTBOX_ROOT} ]]; then
    log_error "${SALTBOX_ROOT} already exists"
    log_error "This installer is for fresh installations only (--repair restores only sb)"
    exit 1
  fi
}

detect_platform() {
  local os arch
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)
  if [[ ${os} != linux ]]; then
    log_error "Unsupported operating system: ${os}"
    log_error "This installer only supports Linux"
    exit 1
  fi
  if [[ ${arch} != x86_64 && ${arch} != amd64 ]]; then
    log_error "Unsupported architecture: ${arch}"
    log_error "This installer only supports x86_64/amd64"
    exit 1
  fi
  log_detail "Detected platform: ${os}/${arch}"
}

# A value from os-release, read without running the file as shell code.
read_os_release_value() {
  local wanted_key="$1" key="" value=""
  # A last line without a newline is read too.
  while IFS='=' read -r key value || [[ -n ${key} ]]; do
    if [[ ${key} == "${wanted_key}" ]]; then
      value="${value%\"}"
      value="${value#\"}"
      value="${value%\'}"
      value="${value#\'}"
      printf '%s\n' "${value}"
      return 0
    fi
  done <"${OS_RELEASE_FILE}"
  return 1
}

# The release, before any temporary file, download or install.
check_ubuntu_support() {
  local distribution_id="" version_id=""
  if [[ ! -r ${OS_RELEASE_FILE} ]]; then
    log_error "Unable to read ${OS_RELEASE_FILE}"
    log_error "This installer only supports Ubuntu"
    exit 1
  fi
  distribution_id=$(read_os_release_value ID || true)
  version_id=$(read_os_release_value VERSION_ID || true)
  if [[ ${distribution_id} != ubuntu ]]; then
    log_error "Unsupported Linux distribution: ${distribution_id:-unknown}"
    log_error "This installer only supports Ubuntu"
    exit 1
  fi
  if [[ ${REPAIR_MODE} == true ]]; then
    case "${version_id}" in
      22.04 | 24.04 | 26.04) ;;
      *)
        log_error "Unsupported Ubuntu version for repair: ${version_id:-unknown}"
        log_error "Supported repair versions: ${RUNTIME_UBUNTU_VERSIONS}"
        exit 1
        ;;
    esac
  else
    case "${version_id}" in
      24.04 | 26.04) ;;
      *)
        log_error "Unsupported Ubuntu version for fresh installation: ${version_id:-unknown}"
        log_error "Supported fresh-install versions: ${FRESH_INSTALL_UBUNTU_VERSIONS}"
        exit 1
        ;;
    esac
  fi
  log_info "Detected supported Ubuntu version: ${version_id}"
}

# A private directory (mktemp's 0700) for the download.
initialize_temp_dir() {
  TEMP_DIR=$(mktemp -d)
}

check_dependencies() {
  local missing_deps=() cmd
  if [[ -n ${FORCE_DOWNLOAD_TOOL} ]]; then
    if ! command -v "${FORCE_DOWNLOAD_TOOL}" >/dev/null 2>&1; then
      log_error "Forced download tool '${FORCE_DOWNLOAD_TOOL}' is not available"
      exit 1
    fi
    DOWNLOAD_TOOL="${FORCE_DOWNLOAD_TOOL}"
    log_detail "Using forced download tool: ${DOWNLOAD_TOOL}"
  elif command -v curl >/dev/null 2>&1; then
    DOWNLOAD_TOOL=curl
    log_detail "Using curl for downloads"
  elif command -v wget >/dev/null 2>&1; then
    DOWNLOAD_TOOL=wget
    log_detail "Using wget for downloads"
  else
    missing_deps+=("curl or wget")
  fi
  for cmd in file mktemp install; do
    if ! command -v "${cmd}" >/dev/null 2>&1; then
      missing_deps+=("${cmd}")
    fi
  done
  if [[ ${#missing_deps[@]} -gt 0 ]]; then
    log_error "Missing required dependencies: ${missing_deps[*]}"
    log_error "Please install them and try again"
    exit 1
  fi
  log_detail "All required dependencies are installed"
}

# `url`'s body on stdout, nothing on stdin, within METADATA_TIMEOUT. curl
# takes https only, redirects included (--proto, --proto-redir:
# https://curl.se/docs/manpage.html#--proto-redir). wget's --https-only
# is documented for recursive retrieval only
# (https://www.gnu.org/software/wget/manual/wget.html#Recursive-Accept_002fReject-Options),
# so with wget a redirect's scheme isn't checked; curl is used whenever
# it is there.
fetch() {
  local url="$1"
  if [[ ${DOWNLOAD_TOOL} == curl ]]; then
    curl --proto =https --proto-redir =https -sSfL \
      --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${METADATA_TIMEOUT}" "${url}" </dev/null
  else
    wget --https-only -qO- --timeout="${METADATA_TIMEOUT}" --tries=1 "${url}" </dev/null
  fi
}

# The `tag_name` in release JSON on stdin, if it is a version.
tag_from_json() {
  local tag
  tag=$(grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' | head -n 1 | sed 's/.*"\([^"]*\)"$/\1/' || true)
  if [[ ${tag} =~ ${TAG_PATTERN} ]]; then
    printf '%s\n' "${tag}"
  fi
}

# The latest release's tag: the svm proxy first (cached, no rate limit),
# then GitHub's API.
get_latest_version() {
  local version=""
  local github_api_url="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
  version=$(fetch "https://svm.saltbox.dev/version?url=${github_api_url}" 2>/dev/null | tag_from_json || true)
  if [[ -z ${version} ]]; then
    log_warn "SVM proxy unavailable, falling back to GitHub API"
    version=$(fetch "${github_api_url}" 2>/dev/null | tag_from_json || true)
    if [[ -z ${version} ]]; then
      log_error "Failed to fetch latest release version from both SVM and GitHub API"
      exit 1
    fi
  fi
  echo "${version}"
}

# The release asset into the temporary directory, from GitHub's releases
# path only (decided.md D-Q2), tried DOWNLOAD_ATTEMPTS times, each try
# ended by a connection that doesn't open or a transfer that stalls; its
# path on stdout.
download_binary() {
  local version="$1"
  local download_url="https://github.com/${GITHUB_REPO}/releases/download/${version}/${DOWNLOAD_BINARY_NAME}"
  local temp_binary="${TEMP_DIR}/${BINARY_NAME}"
  local attempt=1
  log_info "Downloading ${BINARY_NAME} version ${version}..."
  log_detail "Download URL: ${download_url}"
  while true; do
    if [[ ${DOWNLOAD_TOOL} == curl ]]; then
      if curl --proto =https --proto-redir =https -fSL --progress-bar \
        --connect-timeout "${CONNECT_TIMEOUT}" --speed-limit 1024 --speed-time "${STALL_SECONDS}" \
        -o "${temp_binary}" "${download_url}" </dev/null; then
        break
      fi
    elif wget --https-only -q --show-progress --tries=1 --connect-timeout="${CONNECT_TIMEOUT}" \
      --read-timeout="${STALL_SECONDS}" -O "${temp_binary}" "${download_url}" </dev/null; then
      break
    fi
    if [[ ${attempt} -ge ${DOWNLOAD_ATTEMPTS} ]]; then
      log_error "Failed to download binary after ${DOWNLOAD_ATTEMPTS} attempts"
      log_error "Download URL was: ${download_url}"
      exit 1
    fi
    log_warn "Download failed, retrying (${attempt}/${DOWNLOAD_ATTEMPTS})..."
    attempt=$((attempt + 1))
    sleep "${RETRY_DELAY}"
  done
  log_success "Download completed"
  echo "${temp_binary}"
}

# The download is an x86-64 ELF executable of a sane size whose `version`
# reports the tag (without its `v`, as sb's self-update compares) as the
# first line of its stdout; what it writes to stderr (a warning about
# the host's sb config, say) is shown only when the check fails.
verify_binary() {
  local binary_path="$1" expected_version="$2"
  local file_size file_type version_output actual_version status=0
  local errors="${TEMP_DIR}/version.stderr"
  log_info "Verifying downloaded binary..."
  if [[ ! -f ${binary_path} ]]; then
    log_error "Binary file not found at ${binary_path}"
    exit 1
  fi
  file_size=$(stat -c%s "${binary_path}")
  if [[ ${file_size} -lt ${MIN_BINARY_SIZE} ]]; then
    log_error "Binary file size (${file_size} bytes) is too small (expected at least ${MIN_BINARY_SIZE} bytes)"
    log_error "This might indicate a corrupted download or HTML error page"
    exit 1
  fi
  log_detail "Binary size: ${file_size} bytes"
  file_type=$(file -b "${binary_path}")
  if [[ ! ${file_type} =~ ELF\ 64-bit.*executable.*x86-64 ]]; then
    log_error "Downloaded file is not a valid Linux executable"
    log_error "File type: ${file_type}"
    exit 1
  fi
  log_detail "File type: ${file_type}"
  chmod 0755 "${binary_path}"
  log_detail "Testing binary execution..."
  version_output=$("${binary_path}" version 2>"${errors}" </dev/null) || status=$?
  if [[ ${status} -ne 0 ]]; then
    log_error "Binary failed to execute (exit ${status})"
    if [[ ${status} -eq 126 ]]; then
      log_error "It can't be run from ${TEMP_DIR%/*}: is it mounted noexec?"
    fi
    log_error "Output: ${version_output} $(cat -- "${errors}")"
    exit 1
  fi
  version_output="${version_output%%$'\n'*}"
  log_detail "Version output: ${version_output}"
  # `Saltbox CLI version: X.Y.Z (commit: abc1234)`, sb's and sb-go's line.
  if [[ ! ${version_output} =~ ^Saltbox\ CLI\ version:\ ([^ ]+)\ \(commit:\ .+\)$ ]]; then
    log_error "Version output format is incorrect!"
    log_error "Expected format: 'Saltbox CLI version: X.X.X (commit: xxxxx)'"
    log_error "Got: ${version_output} $(cat -- "${errors}")"
    exit 1
  fi
  actual_version="${BASH_REMATCH[1]}"
  if [[ ${actual_version} != "${expected_version#v}" && ${actual_version} != "${expected_version}" ]]; then
    log_error "Version mismatch!"
    log_error "Expected version: ${expected_version}"
    log_error "Binary reports version: ${actual_version}"
    exit 1
  fi
  log_success "Binary verification passed (version: ${actual_version})"
}

# The binary into INSTALL_PATH, root's and 0755, by a rename in its
# directory (never into a directory at that path: `mv -T`), synced: a
# running sb is never written over, and no half-copied file is ever at
# the path. An interrupted run leaves no staged file (cleanup).
install_binary() {
  local binary_path="$1"
  STAGED="${INSTALL_PATH}.install.$$"
  log_info "Installing binary to ${INSTALL_PATH}..."
  if [[ -e ${INSTALL_PATH} ]]; then
    log_warn "Existing binary found at ${INSTALL_PATH}, replacing it..."
  fi
  if ! install -m 0755 -o root -g root "${binary_path}" "${STAGED}"; then
    log_error "Failed to copy binary to ${INSTALL_PATH}"
    exit 1
  fi
  if ! mv -fT -- "${STAGED}" "${INSTALL_PATH}"; then
    log_error "Failed to move binary into place at ${INSTALL_PATH}"
    exit 1
  fi
  STAGED=""
  sync -- "${INSTALL_PATH}" "${INSTALL_PATH%/*}" || true
  log_success "Binary installed successfully"
}

# `sb setup -b <branch>`, the branch one argument, attached to the
# terminal (never captured, so sb draws its own progress and errors),
# stdin /dev/null so it can never read the piped script. sb's exit code
# is the installer's.
run_setup() {
  local status=0
  log_info "Running setup command..."
  echo "" >&2
  "${INSTALL_PATH}" setup -b "${BRANCH}" </dev/null || status=$?
  if [[ ${status} -ne 0 ]]; then
    log_error "Setup command failed"
    exit "${status}"
  fi
  echo "" >&2
  log_success "Setup completed successfully"
}

usage() {
  cat <<EOF
Usage: $0 [OPTIONS]

Options:
  -b <branch>        Saltbox branch to install (default: master)
  -v, --verbose      Show the installer's own detail lines
  --force-curl       Force using curl for downloads
  --force-wget       Force using wget for downloads
  --repair           Restore only the sb binary on an existing installation
  -h, --help         Show this help message
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case $1 in
      -b)
        # A value starting with `-` is a forgotten branch before another
        # option, refused before anything is downloaded.
        if [[ $# -lt 2 || -z $2 || $2 == -* ]]; then
          log_error "-b needs a branch name"
          exit 1
        fi
        BRANCH="$2"
        shift 2
        ;;
      -v | --verbose)
        VERBOSE_MODE=true
        shift
        ;;
      --force-curl)
        FORCE_DOWNLOAD_TOOL=curl
        shift
        ;;
      --force-wget)
        FORCE_DOWNLOAD_TOOL=wget
        shift
        ;;
      --repair)
        REPAIR_MODE=true
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *)
        log_error "Unknown option: $1"
        echo "Use -h or --help for usage information" >&2
        exit 1
        ;;
    esac
  done
}

main() {
  local version temp_binary
  log_info "Saltbox CLI Installer"
  check_privileges
  if [[ ${REPAIR_MODE} != true ]]; then
    check_saltbox_exists
  fi
  detect_platform
  check_ubuntu_support
  check_dependencies
  initialize_temp_dir
  log_info "Fetching latest release information..."
  version=$(get_latest_version)
  log_info "Latest version: ${version}"
  temp_binary=$(download_binary "${version}")
  verify_binary "${temp_binary}" "${version}"
  install_binary "${temp_binary}"
  if [[ ${REPAIR_MODE} == true ]]; then
    log_success "Saltbox CLI repair complete!"
    log_info "Installed ${BINARY_NAME} to ${INSTALL_PATH} with mode 0755"
    return
  fi
  run_setup
  log_success "Installation complete!"
  log_info "You can now use the '${BINARY_NAME}' command"
}

# Run unless sourced (the policy test sources the functions): `return`
# works at the top level only in a sourced file, never under `bash -s`,
# a pipe or `bash install.sh`, and no environment variable can turn the
# run off. Under `curl ... | bash`, BASH_SOURCE[0] isn't $0, so that
# can't decide it.
if ! (return 0 2>/dev/null); then
  parse_args "$@"
  main
fi

}
