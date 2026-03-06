use crate::procedure::schema::{TextSize, TextColor, FontFamily};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

/// UI configuration for a phase
#[derive(Debug, Clone, Serialize, Deserialize, Type, Default)]
pub struct UiConfig {
    #[serde(default)]
    pub components: Vec<UiComponent>,

    /// Override whether this UI requires user input (shows Continue button).
    /// If not set, auto-detected from component types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_input: Option<bool>,
}

impl UiConfig {
    /// Check if this UI configuration has input components that require user interaction
    pub fn has_input_components(&self) -> bool {
        self.components.iter().any(|comp| comp.is_input_component())
    }

    /// Check if this UI requires user input.
    /// If any input component exists, always requires input (cannot be overridden).
    /// Otherwise, use `requires_input` from the procedure YAML (defaults to false).
    pub fn requires_user_input(&self) -> bool {
        if self.has_input_components() {
            return true;
        }
        self.requires_input.unwrap_or(false)
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
                | ComponentType::Select
                | ComponentType::Multiselect
                | ComponentType::Checklist
                | ComponentType::Switch
                | ComponentType::Slider
                | ComponentType::ImageChoice
                | ComponentType::ImageChecklist
        )
    }

    /// Check if this component is display-only
    pub fn is_display_component(&self) -> bool {
        matches!(
            self.component_type,
            ComponentType::Text
                | ComponentType::Progress
                | ComponentType::Image
                | ComponentType::ArtificialHorizon
        )
    }
}

/// Value type for UI components
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Type)]
#[serde(untagged)]
pub enum ComponentValue {
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<String>),
}

/// Individual UI component
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct UiComponent {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "type")]
    pub component_type: ComponentType,
    pub is_input: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<UiOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>, // Grid columns for image_choice / image_checklist
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>, // Width with unit (e.g., "50%", "100%")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<String>, // Height with unit (e.g., "400px", "600px")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>, // Aspect ratio: "16/9", "4/3", "3/4", "2/3", "9/16", "square", "auto"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit: Option<String>, // Image fit mode: "contain", "cover", "fill"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<ComponentValue>, // Default value for input components (user-facing config)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ComponentValue>, // Runtime value (set by frontend, not from YAML)
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

    // Textarea-specific
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>, // Number of visible text lines for textarea

    // Text prefix/suffix
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>, // Text prefix (e.g., "$", "https://")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>, // Text suffix (e.g., "kg", "%")

    // Text trimming
    #[serde(default = "default_true")]
    pub trim: bool, // Whether to trim whitespace from input (default: true)

    // Text styling (for text display components)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<TextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<TextColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<FontFamily>,
}

fn default_true() -> bool {
    true
}

/// Type of UI component
/// IMPORTANT: Keep in sync with TypeScript UIComponentType in app/types/ui-config.ts
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum ComponentType {
    // Input components (require user interaction)
    TextInput,
    NumberInput,
    Switch,
    Textarea,
    Radio,
    Select,
    Multiselect,
    Checklist,
    Slider,
    ImageChoice,
    ImageChecklist,

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
            "select",
            "multiselect",
            "checklist",
            "slider",
            "image_choice",
            "image_checklist",
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
            "select" => Some(ComponentType::Select),
            "multiselect" => Some(ComponentType::Multiselect),
            "checklist" => Some(ComponentType::Checklist),
            "switch" => Some(ComponentType::Switch),
            "slider" => Some(ComponentType::Slider),
            "image_choice" => Some(ComponentType::ImageChoice),
            "image_checklist" => Some(ComponentType::ImageChecklist),

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
            ComponentType::Select => "select",
            ComponentType::Multiselect => "multiselect",
            ComponentType::Checklist => "checklist",
            ComponentType::Switch => "switch",
            ComponentType::Slider => "slider",
            ComponentType::ImageChoice => "image_choice",
            ComponentType::ImageChecklist => "image_checklist",
            ComponentType::Text => "text",
            ComponentType::Image => "image",
            ComponentType::Progress => "progress",
            ComponentType::ArtificialHorizon => "artificial_horizon",
        }
    }

    /// Check if this component type requires user input
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            ComponentType::TextInput
                | ComponentType::NumberInput
                | ComponentType::Textarea
                | ComponentType::Radio
                | ComponentType::Select
                | ComponentType::Multiselect
                | ComponentType::Checklist
                | ComponentType::Switch
                | ComponentType::Slider
                | ComponentType::ImageChoice
                | ComponentType::ImageChecklist
        )
    }
}

/// Option for select, radio, or button components
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct UiOption {
    pub label: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// UI request data sent to frontend
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct UiRequestData {
    pub request_id: String,
    pub job_id: String,
    pub pipe_path: String,
    pub config: UiConfig,
    pub phase_key: String,
    pub slot_id: Option<String>,
}

// Specta event wrapper for UI request
#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct UiRequestEvent(pub UiRequestData);

/// UI response data from frontend
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct UiResponseData {
    #[serde(flatten)]
    pub values: std::collections::HashMap<String, ResponseValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Value types that can be returned from UI components
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(untagged)]
pub enum ResponseValue {
    String(String),
    Boolean(bool),
    Number(f64),
}

/// Phase result value from Python
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(untagged)]
pub enum PythonPhaseResult {
    Bool(bool),
    String(String),
    Null,
}
