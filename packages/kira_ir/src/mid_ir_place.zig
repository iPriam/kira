//! Pure place and value algebra for the Mid IR ownership checker: how two places
//! relate (same/ancestor/descendant/disjoint/overlap), structural equality, and the
//! small queries the checker uses to classify arguments. These functions depend only
//! on the Mid IR and the semantics model — they hold no dataflow state.
const std = @import("std");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");

pub const PlaceRelation = enum {
    same,
    ancestor,
    descendant,
    disjoint,
    overlap,
};

pub fn rootLocalId(root: mid.Place.Root) ?u32 {
    return switch (root) {
        .local => |id| id,
        .capture => |id| id,
        .return_slot => null,
    };
}

/// A place argument that can be moved out of its source: a local or a chain of
/// struct-field projections. A projection chain that reaches through an array
/// element (`arr[i]`) is excluded, because Kira (like Rust) cannot leave a hole
/// in indexed content; such reads are cloned instead of moved.
pub fn isMovablePlaceValue(value: mid.Value) bool {
    return switch (value) {
        .place => |node| !placeHasIndexProjection(node.place),
        else => false,
    };
}

pub fn placeHasIndexProjection(place: mid.Place) bool {
    for (place.projections) |projection| {
        if (projection == .index) return true;
    }
    return false;
}

pub fn valueType(value: mid.Value) model.ResolvedType {
    return switch (value) {
        .integer => |node| node.ty,
        .float => |node| node.ty,
        .string => |node| node.ty,
        .boolean => |node| node.ty,
        .null_ptr => |node| node.ty,
        .function_ref => |node| node.ty,
        .place => |node| node.place.ty,
        .namespace_ref => |node| node.ty,
        .call => |node| node.ty,
        .virtual_call => |node| node.ty,
        .callback => |node| node.ty,
        .call_value => |node| node.ty,
        .construct => |node| node.ty,
        .construct_enum_variant => |node| node.ty,
        .array => |node| node.ty,
        .builder_array => |node| node.ty,
        .binary => |node| node.ty,
        .unary => |node| node.ty,
        .cast => |node| node.ty,
        .conditional => |node| node.ty,
        .native_state => |node| node.ty,
        .native_user_data => |node| node.ty,
        .native_recover => |node| node.ty,
        .c_string_to_string => |node| node.ty,
        .array_len => |node| node.ty,
        .string_len => |node| node.ty,
        .opaque_member => |node| node.ty,
        .opaque_index => |node| node.ty,
    };
}

pub fn isTriviallyCopyableType(ty: model.ResolvedType) bool {
    return switch (ty.kind) {
        .void, .integer, .float, .boolean, .c_string, .raw_ptr => true,
        else => false,
    };
}

pub fn placeRelation(lhs: mid.Place, rhs: mid.Place) PlaceRelation {
    if (!rootsEqual(lhs.root, rhs.root)) return .disjoint;
    var index: usize = 0;
    while (index < lhs.projections.len and index < rhs.projections.len) : (index += 1) {
        const lhs_projection = lhs.projections[index];
        const rhs_projection = rhs.projections[index];
        switch (lhs_projection) {
            .field => |lhs_field| switch (rhs_projection) {
                .field => |rhs_field| {
                    if (lhs_field.field_index == rhs_field.field_index) continue;
                    return .disjoint;
                },
                else => return .overlap,
            },
            .index => |lhs_index| switch (rhs_projection) {
                .index => |rhs_index| {
                    if (lhs_index.index != null and rhs_index.index != null and lhs_index.index.? == rhs_index.index.?) continue;
                    return .overlap;
                },
                else => return .overlap,
            },
            .parent_view => |lhs_parent| switch (rhs_projection) {
                .parent_view => |rhs_parent| {
                    if (lhs_parent.offset == rhs_parent.offset) continue;
                    return .overlap;
                },
                else => return .overlap,
            },
        }
    }
    if (lhs.projections.len == rhs.projections.len) return .same;
    if (lhs.projections.len < rhs.projections.len) return .ancestor;
    return .descendant;
}

pub fn rootsEqual(lhs: mid.Place.Root, rhs: mid.Place.Root) bool {
    return switch (lhs) {
        .local => |id| switch (rhs) {
            .local => |other| id == other,
            else => false,
        },
        .capture => |id| switch (rhs) {
            .capture => |other| id == other,
            else => false,
        },
        .return_slot => rhs == .return_slot,
    };
}

pub fn placesEqual(lhs: mid.Place, rhs: mid.Place) bool {
    if (!rootsEqual(lhs.root, rhs.root)) return false;
    if (lhs.projections.len != rhs.projections.len) return false;
    for (lhs.projections, rhs.projections) |lhs_projection, rhs_projection| {
        switch (lhs_projection) {
            .field => |lhs_field| switch (rhs_projection) {
                .field => |rhs_field| {
                    if (lhs_field.field_index != rhs_field.field_index) return false;
                },
                else => return false,
            },
            .index => |lhs_index| switch (rhs_projection) {
                .index => |rhs_index| {
                    if (lhs_index.index != rhs_index.index) return false;
                },
                else => return false,
            },
            .parent_view => |lhs_parent| switch (rhs_projection) {
                .parent_view => |rhs_parent| {
                    if (lhs_parent.offset != rhs_parent.offset) return false;
                },
                else => return false,
            },
        }
    }
    return true;
}

pub fn placesEqualOptional(lhs: ?mid.Place, rhs: ?mid.Place) bool {
    if (lhs == null and rhs == null) return true;
    if (lhs == null or rhs == null) return false;
    return placesEqual(lhs.?, rhs.?);
}

pub fn placeSliceContains(items: []const mid.Place, needle: mid.Place) bool {
    for (items) |item| {
        if (placesEqual(item, needle)) return true;
    }
    return false;
}

test {
    std.testing.refAllDecls(@This());
}
