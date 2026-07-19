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
    "-DLLVM_TARGETS_TO_BUILD=$TargetsToBuild"
)

& cmake @configureArgs
& cmake --build $BuildDir --config $BuildType --target install --parallel
