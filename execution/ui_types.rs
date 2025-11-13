use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// UI configuration for a phase
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[derive(Default)]
#[ts(export, rename = "RuntimeUiConfig")]
pub struct UiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub components: Vec<UiComponent>,
}


impl UiConfig {
    /// Check if this UI configuration has input components that require user interaction
    pub fn has_input_components(&self) -> bool {
        self.components.iter().any(|comp| comp.is_input_component())
    }

    /// Check if this UI requires user input (has input components)
    /// If true, orchestrator will wait for user submission
    /// If false, UI is display-only and completes automatically
    pub fn requires_user_input(&self) -> bool {
        self.has_input_components()
    }

    /// Check if this UI should auto-continue (inverse of requires_user_input)
    /// Kept for backward compatibility
    pub fn should_auto_continue(&self) -> bool {
        !self.requires_user_input()
    }
}

impl UiComponent {
    /// Check if this component requires user input
    pub fn is_input_component(&self) -> bool {
        matches!(
            self.component_type,
            ComponentType::TextInput
                | ComponentType::NumberInput
                | ComponentType::Textarea
                | ComponentType::Radio
                | ComponentType::Dropdown
                | ComponentType::Multidropdown
                | ComponentType::Checklist
                | ComponentType::Switch
                | ComponentType::Slider
        )
    }

    /// Check if this component is display-only
    pub fn is_display_component(&self) -> bool {
        matches!(
            self.component_type,
            ComponentType::Text | ComponentType::Progress | ComponentType::Image | ComponentType::ArtificialHorizon
        )
    }
}

/// Value type for UI components
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export, rename = "RuntimeComponentValue")]
#[serde(untagged)]
pub enum ComponentValue {
    Boolean(bool),
    Number(f64),
    String(String),
}

/// Individual UI component
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "RuntimeUiComponent")]
pub struct UiComponent {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "type")]
    pub component_type: ComponentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<UiOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>, // Width with unit (e.g., "50%", "100%")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<String>, // Height with unit (e.g., "400px", "600px")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>, // Aspect ratio: "square", "auto", "16/9", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit: Option<String>, // Image fit mode: "contain", "cover", "fill"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<ComponentValue>, // Default value for input components (user-facing config)
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub value: Option<ComponentValue>, // Runtime value (internal only, not from YAML)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>, // For slider components - minimum value (default 0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>, // For slider and progress components - maximum value (default 100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>, // For slider components - step increment (default 1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>, // Bind UI component value to measurement (e.g., "measurement.voltage")

    // Text input validation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>, // Minimum character length for text input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>, // Maximum character length for text input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>, // Regex pattern for text input validation

    // Text prefix/suffix
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>, // Text prefix (e.g., "$", "https://")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>, // Text suffix (e.g., "kg", "%")

    // Text trimming
    #[serde(default = "default_true")]
    pub trim: bool, // Whether to trim whitespace from input (default: true)
}

fn default_true() -> bool {
    true
}

/// Type of UI component
/// IMPORTANT: Keep in sync with TypeScript UIComponentType in app/types/ui-config.ts
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, rename = "RuntimeComponentType")]
#[serde(rename_all = "snake_case")]
pub enum ComponentType {
    // Input components (require user interaction)
    TextInput,
    NumberInput,
    Switch,
    Textarea,
    Radio,
    Dropdown,
    Multidropdown,
    Checklist,
    Slider,

    // Display components (output only)
    Text,
    Image,
    Progress,
    ArtificialHorizon,
}

impl ComponentType {
    /// Get all valid component type strings
    /// MUST match TypeScript UIComponentType in app/types/ui-config.ts
    pub const fn all_valid_types() -> &'static [&'static str] {
        &[
            // Input types
            "text_input",
            "number_input",
            "switch",
            "textarea",
            "radio",
            "dropdown",
            "multidropdown",
            "checklist",
            "slider",

            // Display types
            "text",
            "image",
            "progress",
            "artificial_horizon",
        ]
    }

    /// Parse a component type from a string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            // Input types
            "text_input" => Some(ComponentType::TextInput),
            "number_input" => Some(ComponentType::NumberInput),
            "textarea" => Some(ComponentType::Textarea),
            "radio" => Some(ComponentType::Radio),
            "dropdown" => Some(ComponentType::Dropdown),
            "multidropdown" => Some(ComponentType::Multidropdown),
            "checklist" => Some(ComponentType::Checklist),
            "switch" => Some(ComponentType::Switch),
            "slider" => Some(ComponentType::Slider),

            // Display types
            "text" => Some(ComponentType::Text),
            "image" => Some(ComponentType::Image),
            "progress" => Some(ComponentType::Progress),
            "artificial_horizon" => Some(ComponentType::ArtificialHorizon),

            _ => None,
        }
    }

    /// Convert to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            ComponentType::TextInput => "text_input",
            ComponentType::NumberInput => "number_input",
            ComponentType::Textarea => "textarea",
            ComponentType::Radio => "radio",
            ComponentType::Dropdown => "dropdown",
            ComponentType::Multidropdown => "multidropdown",
            ComponentType::Checklist => "checklist",
            ComponentType::Switch => "switch",
            ComponentType::Slider => "slider",
            ComponentType::Text => "text",
            ComponentType::Image => "image",
            ComponentType::Progress => "progress",
            ComponentType::ArtificialHorizon => "artificial_horizon",
        }
    }
}

/// Option for select, radio, or button components
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "RuntimeUiOption")]
pub struct UiOption {
    pub label: String,
    pub value: String,
}

/// UI request data sent to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiRequestData {
    pub request_id: String,
    pub job_id: String,
    pub pipe_path: String,
    pub config: UiConfig,
}

/// UI response data from frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiResponseData {
    #[serde(flatten)]
    pub values: std::collections::HashMap<String, ResponseValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Value types that can be returned from UI components
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseValue {
    String(String),
    Boolean(bool),
    Number(f64),
}

/// Phase result value from Python
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PythonPhaseResult {
    Bool(bool),
    String(String),
    Null,
}
