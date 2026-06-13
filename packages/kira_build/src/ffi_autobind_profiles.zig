const native = @import("kira_native_lib_definition");

pub const ProfileSelection = struct {
    functions: []const []const u8 = &.{},
    structs: []const []const u8 = &.{},
    callbacks: []const []const u8 = &.{},
};

const vulkan_functions = [_][]const u8{
    "vkGetInstanceProcAddr",
    "vkGetDeviceProcAddr",
    "vkEnumerateInstanceVersion",
    "vkEnumerateInstanceExtensionProperties",
    "vkEnumerateInstanceLayerProperties",
    "vkCreateInstance",
    "vkDestroyInstance",
    "vkEnumeratePhysicalDevices",
    "vkGetPhysicalDeviceQueueFamilyProperties",
    "vkGetPhysicalDeviceSurfaceSupportKHR",
    "vkCreateWin32SurfaceKHR",
    "vkCreateXlibSurfaceKHR",
    "vkCreateDevice",
    "vkDestroyDevice",
    "vkGetDeviceQueue",
    "vkCreateSwapchainKHR",
    "vkAcquireNextImageKHR",
    "vkQueuePresentKHR",
};

const vulkan_structs = [_][]const u8{
    "VkAllocationCallbacks",
    "VkApplicationInfo",
    "VkDeviceCreateInfo",
    "VkDeviceQueueCreateInfo",
    "VkExtensionProperties",
    "VkInstanceCreateInfo",
    "VkLayerProperties",
    "VkPhysicalDeviceFeatures",
    "VkQueueFamilyProperties",
    "VkSwapchainCreateInfoKHR",
    "VkWin32SurfaceCreateInfoKHR",
    "VkXlibSurfaceCreateInfoKHR",
};

const vulkan_callbacks = [_][]const u8{
    "PFN_vkAllocationFunction",
    "PFN_vkFreeFunction",
    "PFN_vkInternalAllocationNotification",
    "PFN_vkInternalFreeNotification",
    "PFN_vkReallocationFunction",
    "PFN_vkVoidFunction",
};

// Whole-API dump filters: `-ast-dump-filter` is a substring match and the binder
// additionally restricts indexed declarations to the manifest's header list, so the
// two case variants cover every `vk*` entry point, `Vk*` type/enum, and `PFN_vk*`
// pointer typedef the Vulkan headers declare.
const vulkan_ast_filters = [_][]const u8{
    "vk",
    "Vk",
};

const directx12_functions = [_][]const u8{
    "D3D12CreateDevice",
    "D3D12GetDebugInterface",
    "D3D12SerializeRootSignature",
    "D3D12CreateRootSignatureDeserializer",
    "CreateDXGIFactory2",
};

const directx12_structs = [_][]const u8{
    "D3D12_COMMAND_QUEUE_DESC",
    "D3D12_DESCRIPTOR_HEAP_DESC",
    "D3D12_HEAP_PROPERTIES",
    "D3D12_RESOURCE_DESC",
    "D3D12_RESOURCE_BARRIER",
    "D3D12_ROOT_SIGNATURE_DESC",
    "D3D12_VIEWPORT",
    "D3D12_RECT",
    "DXGI_ADAPTER_DESC1",
    "DXGI_SAMPLE_DESC",
    "DXGI_SWAP_CHAIN_DESC1",
    "GUID",
    "ID3D12CommandQueue",
    "ID3D12CommandQueueVtbl",
    "ID3D12Device",
    "ID3D12DeviceVtbl",
    "IDXGIAdapter1",
    "IDXGIAdapter1Vtbl",
    "IDXGIFactory4",
    "IDXGIFactory4Vtbl",
    "IDXGISwapChain3",
    "IDXGISwapChain3Vtbl",
};

// Whole-API dump filters: D3D12/DXGI entry points, COM interfaces and their vtables,
// descriptor structs, enums, and the shared COM base declarations the interfaces
// reference. The binder's header-list restriction keeps unrelated Windows SDK
// declarations out of the index even though these are substring filters.
const directx12_ast_filters = [_][]const u8{
    "D3D12",
    "D3D_",
    "DXGI",
    "GUID",
    "IID",
    "HRESULT",
    "IUnknown",
    "LUID",
    "LARGE_INTEGER",
    "SECURITY_ATTRIBUTES",
};

pub fn selection(profile: native.AutobindingProfile) ProfileSelection {
    return switch (profile) {
        .generic => .{},
        .vulkan => .{
            .functions = &vulkan_functions,
            .structs = &vulkan_structs,
            .callbacks = &vulkan_callbacks,
        },
        .directx12 => .{
            .functions = &directx12_functions,
            .structs = &directx12_structs,
        },
    };
}

pub fn dynamicLoaderName(profile: native.AutobindingProfile) ?[]const u8 {
    return switch (profile) {
        .generic => null,
        .vulkan => "vulkan",
        .directx12 => "directx12",
    };
}

pub fn astDumpFilters(profile: native.AutobindingProfile) ?[]const []const u8 {
    return switch (profile) {
        .generic => null,
        .vulkan => &vulkan_ast_filters,
        .directx12 => &directx12_ast_filters,
    };
}
