#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: package-llvm.sh --install-dir <path> --output-dir <path> --asset-name <file> --archive-format <zip|tar.xz>
EOF
}

install_dir=""
output_dir=""
asset_name=""
archive_format=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --install-dir)
            install_dir="$2"
            shift 2
            ;;
        --output-dir)
            output_dir="$2"
            shift 2
            ;;
        --asset-name)
            asset_name="$2"
            shift 2
            ;;
        --archive-format)
            archive_format="$2"
            shift 2
            ;;
        *)
            usage
            exit 1
            ;;
    esac
done

if [[ -z "$install_dir" || -z "$output_dir" || -z "$asset_name" || -z "$archive_format" ]]; then
    usage
    exit 1
fi

mkdir -p "$output_dir"
archive_path="$output_dir/$asset_name"
rm -f "$archive_path"

case "$archive_format" in
    tar.xz)
        tar -C "$install_dir" -cJf "$archive_path" .
        ;;
    zip)
        if command -v 7z >/dev/null 2>&1; then
            (
                cd "$install_dir"
                7z a -tzip "$archive_path" . >/dev/null
            )
        elif command -v zip >/dev/null 2>&1; then
            (
                cd "$install_dir"
                zip -q -r "$archive_path" .
            )
        else
            echo "zip packaging requires 7z or zip to be available" >&2
            exit 1
        fi
        ;;
    *)
        echo "unsupported archive format: $archive_format" >&2
        exit 1
        ;;
esac

printf '%s\n' "$archive_path"
