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

# clang is here for `libclang`, which `kira-clang` opens to read C headers.
# The static analyzer and the ARC migrator are a large share of the clang build
# and nothing reaches them through that API; upstream refuses the migrator
# without the analyzer, so they move together.
#
# lld is here because `kira build --target` has to link the object it just
# emitted, and the driver only picks a linker — it does not contain one. Without
# lld in the bundle, clang searches PATH for `ld` and hands an ELF object to
# whatever it finds: on a Windows host that is a PE linker, which answers
# "unrecognised emulation mode: elf_x86_64" and names nothing about the build.
# lld links every format Kira emits from every host it runs on, which is the
# only arrangement under which a target is a property of the compiler rather
# than of the machine it happens to be installed on.
# compiler-rt is here for its sanitizer runtimes: `kira build --sanitize
# address` instruments Kira's own objects and links the runtime FROM THIS
# BUNDLE, never from whatever Xcode or distro clang the host happens to carry.
# Only sanitizer runtimes are built; profiling, fuzzing, and tracing stay out.
cmake -S "$source_dir/llvm" -B "$build_dir" -G "$cmake_generator" \
    -DCMAKE_BUILD_TYPE="$build_type" \
    -DCMAKE_INSTALL_PREFIX="$install_dir" \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DBUILD_SHARED_LIBS=OFF \
    -DLLVM_LINK_LLVM_DYLIB=ON \
    -DLLVM_ENABLE_PROJECTS="clang;lld" \
    -DLLVM_ENABLE_RUNTIMES="compiler-rt" \
    -DCOMPILER_RT_BUILD_SANITIZERS=ON \
    -DCOMPILER_RT_ENABLE_IOS=ON \
    -DCOMPILER_RT_ENABLE_TVOS=ON \
    -DCOMPILER_RT_ENABLE_XROS=ON \
    -DCOMPILER_RT_ENABLE_WATCHOS=OFF \
    -DCOMPILER_RT_BUILD_XRAY=OFF \
    -DCOMPILER_RT_BUILD_LIBFUZZER=OFF \
    -DCOMPILER_RT_BUILD_PROFILE=OFF \
    -DCOMPILER_RT_BUILD_MEMPROF=OFF \
    -DCOMPILER_RT_BUILD_ORC=OFF \
    -DCOMPILER_RT_BUILD_GWP_ASAN=OFF \
    -DCLANG_ENABLE_STATIC_ANALYZER=OFF \
    -DCLANG_ENABLE_ARCMT=OFF \
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
