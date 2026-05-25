const manifest = @import("kira_manifest");

pub const CommandMode = enum {
    check,
    build,
    run,
    live,
};

pub const TargetKind = enum {
    library,
    executable,
    example,
    source_file,
};

pub const Project = struct {
    manifest: manifest.ProjectManifest,
};

pub const ResolvedProject = struct {
    root_path: []const u8,
    manifest_path: []const u8,
    entrypoint_path: []const u8,
    project: Project,
};

pub const ResolvedPackageRoot = struct {
    root_path: []const u8,
    manifest_path: []const u8,
    entrypoint_path: ?[]const u8 = null,
    module_source_root: []const u8,
    project: Project,
};

pub const ResolvedTarget = struct {
    root_path: ?[]const u8 = null,
    manifest_path: ?[]const u8 = null,
    source_path: ?[]const u8 = null,
    source_root: ?[]const u8 = null,
    project_name: ?[]const u8 = null,
    project: ?Project = null,
    package_kind: ?manifest.PackageKind = null,
    target_kind: TargetKind,

    pub fn kindName(self: ResolvedTarget) []const u8 {
        return switch (self.target_kind) {
            .library => "library",
            .executable => "executable",
            .example => "example",
            .source_file => "source_file",
        };
    }

    pub fn displayPath(self: ResolvedTarget) []const u8 {
        return self.root_path orelse self.source_path orelse ".";
    }

    pub fn canCheck(self: ResolvedTarget) bool {
        _ = self;
        return true;
    }

    pub fn canBuild(self: ResolvedTarget) bool {
        _ = self;
        return true;
    }

    pub fn canRun(self: ResolvedTarget) bool {
        return switch (self.target_kind) {
            .library => false,
            .executable, .example, .source_file => self.source_path != null,
        };
    }

    pub fn canLive(self: ResolvedTarget) bool {
        return switch (self.target_kind) {
            .example, .executable => self.source_path != null,
            .library, .source_file => false,
        };
    }
};
