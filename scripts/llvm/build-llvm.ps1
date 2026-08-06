param(
    [Parameter(Mandatory = $true)]
    [string]$SourceDir,
    [Parameter(Mandatory = $true)]
    [string]$BuildDir,
    [Parameter(Mandatory = $true)]
    [string]$InstallDir,
    [Parameter(Mandatory = $true)]
    [string]$TargetKey,
    [Parameter(Mandatory = $true)]
    [string]$BuildType,
    [Parameter(Mandatory = $true)]
    [string]$CmakeGenerator,
    [Parameter(Mandatory = $true)]
    [string]$TargetsToBuild
)

$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$configureArgs = @(
    "-S", (Join-Path $SourceDir "llvm"),
    "-B", $BuildDir,
    "-G", $CmakeGenerator,
    "-DCMAKE_BUILD_TYPE=$BuildType",
    "-DCMAKE_INSTALL_PREFIX=$InstallDir",
    "-DCMAKE_INSTALL_LIBDIR=lib",
    "-DBUILD_SHARED_LIBS=OFF",
    "-DLLVM_ENABLE_PROJECTS=clang",
    "-DLLVM_ENABLE_BINDINGS=OFF",
    "-DLLVM_ENABLE_LIBXML2=OFF",
    "-DLLVM_ENABLE_ZLIB=OFF",
    "-DLLVM_ENABLE_ZSTD=OFF",
    "-DLLVM_INCLUDE_BENCHMARKS=OFF",
    "-DLLVM_INCLUDE_DOCS=OFF",
    "-DLLVM_INCLUDE_EXAMPLES=OFF",
    "-DLLVM_INCLUDE_TESTS=OFF",
    "-DLLVM_BUILD_TOOLS=ON",
    "-DLLVM_TARGETS_TO_BUILD=$TargetsToBuild",
    # Both link shapes ship, because both are used: the backend links the
    # component archives into `kira`, and `kira-clang` opens `libclang` out of
    # the same tree at runtime. MSVC has no `libLLVM` dylib, so the Unix
    # script's `LLVM_LINK_LLVM_DYLIB` has no counterpart here and the C API
    # ships as `LLVM-C.dll` instead. Named rather than left to the upstream
    # defaults they currently match, because the bundle's contents are a
    # contract with `kira_clang::libclang_candidates` and not an incidental.
    "-DLLVM_BUILD_LLVM_C_DYLIB=ON",
    "-DLIBCLANG_BUILD_STATIC=OFF",
    # The MSVC STL routes several algorithms through helpers (`__std_rotate`
    # and friends) that live in the toolset's own library, so a bundle built
    # against a newer STL than the consumer's fails to link naming symbols no
    # released Visual Studio defines. This bundle is redistributable: it is
    # linked on developer machines whose toolset nobody controls. Opting out
    # keeps the generic templates, which resolve entirely within the headers,
    # and `check-msvc-portability.ps1` fails the build if the STL ever routes
    # something new through a helper this flag does not cover.
    "-DCMAKE_CXX_FLAGS=/D_USE_STD_VECTOR_ALGORITHMS=0"
)

& cmake @configureArgs
& cmake --build $BuildDir --config $BuildType --target install --parallel
