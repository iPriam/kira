const zig_cli = @import("cli");

pub const dependency_name = "sam701/zig-cli";
pub const dependency_branch = "zig-0.15";

pub fn dependencyAvailable() bool {
    return @hasDecl(zig_cli, "App") and @hasDecl(zig_cli, "Command");
}
