const std = @import("std");

pub fn addManagedToolchainInstallStep(
    b: *std.Build,
    host: std.Target,
    cli: *std.Build.Step.Compile,
    bootstrapper: *std.Build.Step.Compile,
    version: []const u8,
    channel: []const u8,
    metadata_file: std.Build.LazyPath,
    templates_dir: std.Build.LazyPath,
    foundation_dir: std.Build.LazyPath,
    bootstrapper_install_dir: []const u8,
) *std.Build.Step.Run {
    const step = switch (host.os.tag) {
        .windows => b.addSystemCommand(&.{
            "powershell.exe",
            "-NoProfile",
            "-Command",
            b.fmt(
                "& {{ param([string]$cliSource, [string]$bootstrapperSource, [string]$version, [string]$channel, [string]$metadataSource, [string]$templatesSource, [string]$foundationSource, [string]$bootstrapperBinDir); " ++
                    "$ErrorActionPreference = 'Stop'; " ++
                    "$kiraHome = Join-Path $HOME '.kira'; " ++
                    "$toolchainRoot = Join-Path $kiraHome ('toolchains\\' + $channel + '\\' + $version); " ++
                    "if (Test-Path $toolchainRoot) {{ Remove-Item $toolchainRoot -Recurse -Force; }}; " ++
                    "$binDir = Join-Path $toolchainRoot 'bin'; " ++
                    "New-Item -ItemType Directory -Force -Path $binDir | Out-Null; " ++
                    "$kiracDest = Join-Path $binDir 'kirac.exe'; " ++
                    "Copy-Item $cliSource $kiracDest -Force; " ++
                    "(Get-Item $kiracDest).LastWriteTime = Get-Date; " ++
                    "$pdbSource = [System.IO.Path]::ChangeExtension($cliSource, 'pdb'); " ++
                    "if (Test-Path $pdbSource) {{ $kiracPdbDest = Join-Path $binDir 'kirac.pdb'; Copy-Item $pdbSource $kiracPdbDest -Force; (Get-Item $kiracPdbDest).LastWriteTime = Get-Date; }}; " ++
                    "New-Item -ItemType Directory -Force -Path $bootstrapperBinDir | Out-Null; " ++
                    "$bootstrapperDest = Join-Path $bootstrapperBinDir 'kira-bootstrapper.exe'; " ++
                    "$kiraDest = Join-Path $bootstrapperBinDir 'kira.exe'; " ++
                    "if ([System.IO.Path]::GetFullPath($bootstrapperSource) -ne [System.IO.Path]::GetFullPath($bootstrapperDest)) {{ Copy-Item $bootstrapperSource $bootstrapperDest -Force }}; " ++
                    "Copy-Item $bootstrapperSource $kiraDest -Force; " ++
                    "(Get-Item $bootstrapperDest).LastWriteTime = Get-Date; " ++
                    "(Get-Item $kiraDest).LastWriteTime = Get-Date; " ++
                    "$bootstrapperPdbSource = [System.IO.Path]::ChangeExtension($bootstrapperSource, 'pdb'); " ++
                    "if (Test-Path $bootstrapperPdbSource) {{ $bootstrapperPdbDest = Join-Path $bootstrapperBinDir 'kira-bootstrapper.pdb'; $kiraPdbDest = Join-Path $bootstrapperBinDir 'kira.pdb'; if ([System.IO.Path]::GetFullPath($bootstrapperPdbSource) -ne [System.IO.Path]::GetFullPath($bootstrapperPdbDest)) {{ Copy-Item $bootstrapperPdbSource $bootstrapperPdbDest -Force }}; Copy-Item $bootstrapperPdbSource $kiraPdbDest -Force; (Get-Item $bootstrapperPdbDest).LastWriteTime = Get-Date; (Get-Item $kiraPdbDest).LastWriteTime = Get-Date; }}; " ++
                    "Copy-Item $metadataSource (Join-Path $toolchainRoot 'llvm-metadata.toml') -Force; " ++
                    "$templatesDest = Join-Path $toolchainRoot 'templates'; " ++
                    "if (Test-Path $templatesDest) {{ Remove-Item $templatesDest -Recurse -Force; }}; " ++
                    "Copy-Item $templatesSource $templatesDest -Recurse -Force; " ++
                    "$foundationDest = Join-Path $toolchainRoot 'foundation'; " ++
                    "if (Test-Path $foundationDest) {{ Remove-Item $foundationDest -Recurse -Force; }}; " ++
                    "Copy-Item $foundationSource $foundationDest -Recurse -Force; " ++
                    "$currentDir = Join-Path $kiraHome 'toolchains'; " ++
                    "New-Item -ItemType Directory -Force -Path $currentDir | Out-Null; " ++
                    "$currentPath = Join-Path $currentDir 'current.toml'; " ++
                    "Set-Content -Path $currentPath -Value ('channel = \"' + $channel + '\"'); " ++
                    "Add-Content -Path $currentPath -Value ('version = \"' + $version + '\"'); " ++
                    "Add-Content -Path $currentPath -Value 'primary = \"kirac\"'; " ++
                    "$normalize = {{ param([string]$value) if ([string]::IsNullOrWhiteSpace($value)) {{ return '' }} return $value.Trim().TrimEnd([char[]]@(92, 47)).ToLowerInvariant() }}; " ++
                    "$target = & $normalize $bootstrapperBinDir; " ++
                    "$userPath = [Environment]::GetEnvironmentVariable('Path', 'User'); " ++
                    "$entries = @(); " ++
                    "$entries += $env:Path -split ';' | Where-Object {{ -not [string]::IsNullOrWhiteSpace($_) }}; " ++
                    "if (-not [string]::IsNullOrWhiteSpace($userPath)) {{ $entries += $userPath -split ';' | Where-Object {{ -not [string]::IsNullOrWhiteSpace($_) }} }}; " ++
                    "$exists = $false; " ++
                    "foreach ($entry in $entries) {{ if ((& $normalize $entry) -eq $target) {{ $exists = $true; break }} }}; " ++
                    "if (-not $exists) {{ " ++
                    "  $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) {{ $bootstrapperBinDir }} else {{ $userPath.TrimEnd(';') + ';' + $bootstrapperBinDir }}; " ++
                    "  [Environment]::SetEnvironmentVariable('Path', $newPath, 'User'); " ++
                    "}}; " ++
                    "Write-Host ('Installed Kira toolchain to ' + $toolchainRoot); " ++
                    "Write-Host ('Activated ' + $channel + '/' + $version); " ++
                    "if ($exists) {{ Write-Host 'kira-bootstrapper is already in your PATH. Restart your shell if needed.' }} else {{ Write-Host 'Added kira-bootstrapper to your PATH. Restart your shell to use it.' }} }}",
                .{},
            ),
        }),
        else => b.addSystemCommand(&.{
            "sh",
            "-c",
            b.fmt(
                "set -eu; " ++
                    "cli_source=\"$0\"; bootstrapper_source=\"$1\"; version=\"$2\"; channel=\"$3\"; metadata_source=\"$4\"; templates_source=\"$5\"; foundation_source=\"$6\"; bootstrapper_bin_dir=\"$7\"; " ++
                    "kira_home=\"$HOME/.kira\"; toolchain_root=\"$kira_home/toolchains/$channel/$version\"; bin_dir=\"$toolchain_root/bin\"; " ++
                    "rm -rf \"$toolchain_root\"; " ++
                    "mkdir -p \"$bin_dir\"; " ++
                    "cp \"$cli_source\" \"$bin_dir/kirac\"; chmod +x \"$bin_dir/kirac\"; touch \"$bin_dir/kirac\"; " ++
                    "mkdir -p \"$bootstrapper_bin_dir\"; " ++
                    "if [ \"$bootstrapper_source\" != \"$bootstrapper_bin_dir/kira-bootstrapper\" ]; then cp \"$bootstrapper_source\" \"$bootstrapper_bin_dir/kira-bootstrapper\"; fi; chmod +x \"$bootstrapper_bin_dir/kira-bootstrapper\"; touch \"$bootstrapper_bin_dir/kira-bootstrapper\"; " ++
                    "cp \"$bootstrapper_source\" \"$bootstrapper_bin_dir/kira\"; chmod +x \"$bootstrapper_bin_dir/kira\"; touch \"$bootstrapper_bin_dir/kira\"; " ++
                    "cp \"$metadata_source\" \"$toolchain_root/llvm-metadata.toml\"; " ++
                    "rm -rf \"$toolchain_root/templates\"; cp -R \"$templates_source\" \"$toolchain_root/templates\"; " ++
                    "rm -rf \"$toolchain_root/foundation\"; cp -R \"$foundation_source\" \"$toolchain_root/foundation\"; " ++
                    "mkdir -p \"$kira_home/toolchains\"; " ++
                    "cat > \"$kira_home/toolchains/current.toml\" <<EOF\nchannel = \"$channel\"\nversion = \"$version\"\nprimary = \"kirac\"\nEOF\n" ++
                    "path_added=0; " ++
                    "case \":$PATH:\" in *\":$bootstrapper_bin_dir:\"*) path_exists=1 ;; *) path_exists=0 ;; esac; " ++
                    "if [ \"$path_exists\" -eq 0 ]; then " ++
                    "  shell_name=$(basename \"${{SHELL:-}}\"); " ++
                    "  case \"$shell_name\" in zsh) rc_file=\"$HOME/.zshrc\" ;; bash) rc_file=\"$HOME/.bashrc\" ;; *) if [ \"$(uname -s)\" = Darwin ]; then rc_file=\"$HOME/.zshrc\"; else rc_file=\"$HOME/.profile\"; fi ;; esac; " ++
                    "  line=\"export PATH=\\\"$bootstrapper_bin_dir:\\$PATH\\\"\"; touch \"$rc_file\"; " ++
                    "  if ! grep -Fqx \"$line\" \"$rc_file\"; then printf '\\n%s\\n' \"$line\" >> \"$rc_file\"; path_added=1; fi; " ++
                    "fi; " ++
                    "printf '%s\\n' \"Installed Kira toolchain to $toolchain_root\"; " ++
                    "printf '%s\\n' \"Activated $channel/$version\"; " ++
                    "if [ \"$path_exists\" -eq 1 ] || [ \"$path_added\" -eq 0 -a \"$path_exists\" -eq 0 ]; then printf '%s\\n' 'kira-bootstrapper is already in your PATH. Restart your shell if needed.'; else printf '%s\\n' 'Added kira-bootstrapper to your PATH. Restart your shell to use it.'; fi",
                .{},
            ),
        }),
    };
    step.has_side_effects = true;
    step.addArtifactArg(cli);
    step.addArtifactArg(bootstrapper);
    step.addArg(version);
    step.addArg(channel);
    step.addFileArg(metadata_file);
    step.addFileArg(templates_dir);
    step.addFileArg(foundation_dir);
    step.addArg(bootstrapper_install_dir);
    return step;
}
