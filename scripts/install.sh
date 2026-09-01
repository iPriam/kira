#!/bin/sh
# The `curl | sh` bootstrap: puts `knvm` and the `kira` launcher on PATH.
#
#   curl -fsSL https://kira-lang.com/install.sh | sh
#
# This is the only step that cannot use Kira's own tooling, because it runs on
# a machine that has none. It fetches the published tools archive
# (`knvm-<version>-<host-key>.tar.gz`), verifies it against the checksum
# published beside it, unpacks it into `<kira-home>/bin`, and configures the
# shell the same way `knvm sinstall` does. It installs no toolchain: the last
# thing it prints is `knvm install latest`, which is a decision left to the
# user rather than made for them by a pipe from the internet.
#
# Environment:
#   KIRA_HOME      where the tools land (default: ~/.kira)
#   KIRA_VERSION   which release to install (default: the newest)
#   KIRA_REPO      the repository to fetch from (default: kira-lang-com/kira)
#   KIRA_NO_MODIFY_PATH   set to any value to skip editing a startup file
#   GH_TOKEN, GITHUB_TOKEN   sent to GitHub, for the authenticated rate limit

set -eu

repo="${KIRA_REPO:-kira-lang-com/kira}"
kira_home="${KIRA_HOME:-$HOME/.kira}"
bin_dir="$kira_home/bin"

say() { printf 'kira: %s\n' "$1"; }
fail() { printf 'kira: %s\n' "$1" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || fail "\`$1\` is required and was not found on PATH"
}

need curl
need tar
need uname

# The host key set is the one `kira-toolchain` uses for every managed artifact;
# a host outside it has no published build, and saying so beats downloading
# something that cannot run.
host_key() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os/$arch" in
        Darwin/arm64) echo "aarch64-macos" ;;
        Darwin/x86_64) echo "x86_64-macos" ;;
        Linux/x86_64) echo "x86_64-linux-gnu" ;;
        Linux/aarch64|Linux/arm64) echo "aarch64-linux-gnu" ;;
        MINGW*/*|MSYS*/*|CYGWIN*/*)
            case "$arch" in
                x86_64|amd64) echo "x86_64-windows-msvc" ;;
                aarch64|arm64) echo "aarch64-windows-msvc" ;;
                *) fail "no Kira build is published for $os/$arch; build from a checkout with \`cargo run -p kira-knvm -- sinstall\`" ;;
            esac
            ;;
        *) fail "no Kira build is published for $os/$arch; build from a checkout with \`cargo run -p kira-knvm -- sinstall\`" ;;
    esac
}

# Unauthenticated GitHub allows sixty API requests an hour per address, which
# is shared: behind one office address or on a CI runner, this bootstrap fails
# on traffic that is not yours. Every such environment already has a token, and
# the two variables read here are the ones `gh` itself resolves, in its order.
# Only GitHub is sent it: `curl` drops the header when a download redirects to
# the storage host that serves an asset's bytes.
token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
if [ -n "$token" ]; then
    set -- -H "Authorization: Bearer $token"
else
    set --
fi

# The newest Kira release, read from the releases API, which returns releases
# newest first.
#
# The feed carries every release this repository publishes, and the managed
# LLVM bundles are published here too, under `llvm-v<version>-kira.<n>` tags.
# Taking the newest `tag_name` outright resolved one of those and then asked for
# a Kira archive underneath it, which 404s — so the tag has to be one of Kira's
# own: `v` and a dotted number. That also excludes `v1.8.0-dev5` and its kind,
# for the reason `knvm install latest` defaults to the release channel.
#
# `grep -o` puts one tag on each line before any of that is decided, because the
# API answers with the whole feed on one line and a greedy `.*` across it names
# whichever release happens to be last.
#
# The authorization arguments are passed in rather than read from the script's
# positional parameters: inside a function `$@` is the function's own argument
# list, so a header set at script scope would silently not be sent.
newest_version() {
    curl -fsSL \
        -H 'User-Agent: kira-install' \
        -H 'Accept: application/vnd.github+json' \
        "$@" \
        "https://api.github.com/repos/$repo/releases?per_page=100" |
        grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' |
        sed -n 's/.*"v\([0-9][0-9.]*\)"$/\1/p' |
        head -1
}

# `shasum -a 256` on macOS, `sha256sum` on Linux. A host with neither cannot
# verify the download, which is reported rather than passed over.
digest_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        echo ""
    fi
}

key="$(host_key)"
version="${KIRA_VERSION:-$(newest_version "$@")}"
[ -n "$version" ] || fail "could not resolve the newest release from $repo"

asset="knvm-$version-$key.tar.gz"
base="https://github.com/$repo/releases/download/v$version"

work="$(mktemp -d)"
# Removed however this exits, so an interrupted bootstrap leaves nothing behind.
trap 'rm -rf "$work"' EXIT INT TERM

say "downloading $asset"
curl -fsSL --max-time 300 "$@" -o "$work/$asset" "$base/$asset" ||
    fail "could not download $base/$asset"

if curl -fsSL --max-time 60 "$@" -o "$work/$asset.sha256" "$base/$asset.sha256" 2>/dev/null; then
    published="$(cut -d' ' -f1 <"$work/$asset.sha256")"
    actual="$(digest_of "$work/$asset")"
    if [ -z "$actual" ]; then
        say "warning: neither shasum nor sha256sum is available; installed unverified"
    elif [ "$published" != "$actual" ]; then
        fail "checksum mismatch for $asset
  published: $published
  downloaded: $actual
The download is corrupt or the archive was changed after publication; nothing was installed"
    else
        say "verified sha256 $actual"
    fi
else
    say "warning: no checksum is published for $asset; installed unverified"
fi

mkdir -p "$work/unpacked" "$bin_dir"
tar -xzf "$work/$asset" -C "$work/unpacked"

# The archive wraps its payload in one `knvm-<version>/` directory; accept a
# flat one too rather than depending on which.
payload="$work/unpacked/knvm-$version"
[ -d "$payload/bin" ] || payload="$work/unpacked"
[ -d "$payload/bin" ] || fail "$asset does not contain a bin/ directory"

for tool in knvm kira kira-language-server; do
    [ -f "$payload/bin/$tool" ] || fail "$asset has no bin/$tool"
done
for tool in knvm kira kira-language-server; do
    # Staged then renamed, so a bootstrap run from a shell whose PATH already
    # holds these replaces them without writing onto a busy binary.
    cp "$payload/bin/$tool" "$bin_dir/.incoming-$tool"
    chmod 755 "$bin_dir/.incoming-$tool"
    mv "$bin_dir/.incoming-$tool" "$bin_dir/$tool"
done
say "installed knvm, kira, and kira-language-server into $bin_dir"

env_script="$kira_home/env"
cat >"$env_script" <<EOF
# Added by the Kira installer: puts the kira tools on PATH.
export PATH="$bin_dir:\$PATH"
EOF

if [ -n "${KIRA_NO_MODIFY_PATH:-}" ]; then
    say "not editing any startup file; source $env_script yourself"
else
    # The file the user's shell actually reads, chosen from $SHELL rather than
    # from what happens to exist: a default macOS home has no dotfiles, and a
    # line in .profile configures nothing for the zsh that machine runs.
    case "$(basename "${SHELL:-}")" in
        zsh) startup="$HOME/.zshenv" ;;
        bash) startup="$HOME/.bashrc" ;;
        *) startup="$HOME/.profile" ;;
    esac
    line=". \"$env_script\""
    if [ -f "$startup" ] && grep -qF "$line" "$startup"; then
        say "$startup already sources the env script"
    else
        printf '%s\n' "$line" >>"$startup"
        say "added a PATH line to $startup"
    fi
fi

say "done. Open a new shell, or run: . \"$env_script\""
say "then install a toolchain with: knvm install latest"
