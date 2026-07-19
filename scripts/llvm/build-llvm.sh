#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: build-llvm.sh --source-dir <path> --build-dir <path> --install-dir <path> --target-key <key> --build-type <Release> --cmake-generator <Ninja> --targets-to-build <host>
EOF
}

source_dir=""
build_dir=""
install_dir=""
target_key=""
build_type=""
cmake_generator=""
targets_to_build=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source-dir)
            source_dir="$2"
            shift 2
            ;;
        --build-dir)
            build_dir="$2"
            shift 2
            ;;
        --install-dir)
            install_dir="$2"
            shift 2
            ;;
        --target-key)
            target_key="$2"
            shift 2
            ;;
        --build-type)
            build_type="$2"
            shift 2
            ;;
        --cmake-generator)
            cmake_generator="$2"
            shift 2
            ;;
        --targets-to-build)
            targets_to_build="$2"
            shift 2
            ;;
        *)
            usage
            exit 1
            ;;
    esac
done

if [[ -z "$source_dir" || -z "$build_dir" || -z "$install_dir" || -z "$target_key" || -z "$build_type" || -z "$cmake_generator" || -z "$targets_to_build" ]]; then
    usage
    exit 1
fi

mkdir -p "$build_dir" "$install_dir"

cmake -S "$source_dir/llvm" -B "$build_dir" -G "$cmake_generator" \
    -DCMAKE_BUILD_TYPE="$build_type" \
    -DCMAKE_INSTALL_PREFIX="$install_dir" \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DBUILD_SHARED_LIBS=OFF \
    -DLLVM_LINK_LLVM_DYLIB=ON \
    -DLLVM_ENABLE_PROJECTS=clang \
    -DLLVM_ENABLE_BINDINGS=OFF \
    -DLLVM_ENABLE_LIBXML2=OFF \
    -DLLVM_ENABLE_ZLIB=OFF \
    -DLLVM_ENABLE_ZSTD=OFF \
    -DLLVM_INCLUDE_BENCHMARKS=OFF \
    -DLLVM_INCLUDE_DOCS=OFF \
    -DLLVM_INCLUDE_EXAMPLES=OFF \
    -DLLVM_INCLUDE_TESTS=OFF \
    -DLLVM_BUILD_TOOLS=ON \
    -DLLVM_TARGETS_TO_BUILD="$targets_to_build"

cmake --build "$build_dir" --config "$build_type" --target install --parallel
