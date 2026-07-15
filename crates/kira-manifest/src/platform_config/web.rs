//! Web surface/rendering model and surface requirements.

/// The graphics bridge used by Web execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebGraphicsBridge {
    #[default]
    None,
    Webgpu,
}

impl WebGraphicsBridge {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "webgpu" => Some(Self::Webgpu),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Webgpu => "webgpu",
        }
    }
}

/// The rendering surface a Web app targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSurface {
    Dom,
    Webgpu,
    Hybrid,
}

impl WebSurface {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dom" => Some(Self::Dom),
            "webgpu" => Some(Self::Webgpu),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dom => "dom",
            Self::Webgpu => "webgpu",
            Self::Hybrid => "hybrid",
        }
    }
}

/// How a Web surface renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRenderingModel {
    Dom,
    GraphicsCanvas,
    Hybrid,
}

impl WebRenderingModel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dom => "dom",
            Self::GraphicsCanvas => "graphics-canvas",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Graphics capabilities a Web surface may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebGraphicsCapability {
    Webgpu,
}

impl WebGraphicsCapability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Webgpu => "webgpu",
        }
    }
}

/// What a chosen Web surface requires from the host page/browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSurfaceRequirements {
    pub surface: WebSurface,
    pub rendering_model: WebRenderingModel,
    pub graphics_capability: Option<WebGraphicsCapability>,
    pub requires_canvas: bool,
    pub requires_browser_detection: bool,
}

/// Resolve the requirements implied by a Web surface choice.
pub fn web_surface_requirements(surface: WebSurface) -> WebSurfaceRequirements {
    match surface {
        WebSurface::Dom => WebSurfaceRequirements {
            surface: WebSurface::Dom,
            rendering_model: WebRenderingModel::Dom,
            graphics_capability: None,
            requires_canvas: false,
            requires_browser_detection: false,
        },
        WebSurface::Webgpu => WebSurfaceRequirements {
            surface: WebSurface::Webgpu,
            rendering_model: WebRenderingModel::GraphicsCanvas,
            graphics_capability: Some(WebGraphicsCapability::Webgpu),
            requires_canvas: true,
            requires_browser_detection: true,
        },
        WebSurface::Hybrid => WebSurfaceRequirements {
            surface: WebSurface::Hybrid,
            rendering_model: WebRenderingModel::Hybrid,
            graphics_capability: Some(WebGraphicsCapability::Webgpu),
            requires_canvas: true,
            requires_browser_detection: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_surface_requirements_distinguish_dom_and_webgpu() {
        let dom = web_surface_requirements(WebSurface::Dom);
        assert_eq!(WebRenderingModel::Dom, dom.rendering_model);
        assert!(!dom.requires_canvas);
        assert!(dom.graphics_capability.is_none());

        let webgpu = web_surface_requirements(WebSurface::Webgpu);
        assert_eq!(WebRenderingModel::GraphicsCanvas, webgpu.rendering_model);
        assert_eq!(
            Some(WebGraphicsCapability::Webgpu),
            webgpu.graphics_capability
        );
        assert!(webgpu.requires_canvas);
        assert!(webgpu.requires_browser_detection);
    }
}
