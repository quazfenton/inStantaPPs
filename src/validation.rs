//! Input Validation Module
//!
//! Provides request validation for API endpoints.
//!
//! # Validated Fields
//!
//! - State IDs (format, length)
//! - Labels (length, allowed characters)
//! - TTL values (range, format)
//! - VM configurations (resource limits)
//! - File paths (no path traversal)
//!
//! # Usage
//!
//! ```rust,no_run
//! use isa_workspace::validation::{validate_state_id, validate_label, ValidationErrors};
//!
//! // Validate state ID
//! let result = validate_state_id(&input.state_id);
//! if let Err(e) = result {
//!     return Err(ApiError::Validation(e));
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Validation error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    /// Field that failed validation
    pub field: String,
    /// Error message
    pub message: String,
    /// Validation rule that failed
    pub rule: ValidationRule,
}

/// Validation rules
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationRule {
    Required,
    MinLength,
    MaxLength,
    Format,
    Range,
    Pattern,
    PathTraversal,
    InvalidCharacter,
}

/// Validation errors collection
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ValidationErrors {
    pub errors: Vec<ValidationError>,
}

impl ValidationErrors {
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    pub fn add(&mut self, field: &str, message: &str, rule: ValidationRule) {
        self.errors.push(ValidationError {
            field: field.to_string(),
            message: message.to_string(),
            rule,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn into_error(self) -> Option<Self> {
        if self.is_empty() {
            None
        } else {
            Some(self)
        }
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, error) in self.errors.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}: {}", error.field, error.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Validate state ID format
///
/// Rules:
/// - Must be non-empty
/// - Must be a valid UUID or alphanumeric string
/// - Maximum 64 characters
pub fn validate_state_id(state_id: &str) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();

    if state_id.is_empty() {
        errors.add("state_id", "State ID is required", ValidationRule::Required);
        return Err(errors);
    }

    if state_id.len() > 64 {
        errors.add(
            "state_id",
            "State ID must be 64 characters or less",
            ValidationRule::MaxLength,
        );
    }

    // Check for valid characters (alphanumeric, hyphen, underscore)
    if !state_id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        errors.add(
            "state_id",
            "State ID must contain only alphanumeric characters, hyphens, and underscores",
            ValidationRule::Pattern,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate label format
///
/// Rules:
/// - Must be non-empty
/// - Maximum 256 characters
/// - Allowed characters: alphanumeric, spaces, hyphens, underscores
pub fn validate_label(label: &str) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();

    if label.is_empty() {
        errors.add("label", "Label is required", ValidationRule::Required);
        return Err(errors);
    }

    if label.len() > 256 {
        errors.add(
            "label",
            "Label must be 256 characters or less",
            ValidationRule::MaxLength,
        );
    }

    // Check for valid characters
    if !label.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_') {
        errors.add(
            "label",
            "Label must contain only alphanumeric characters, spaces, hyphens, and underscores",
            ValidationRule::Pattern,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate TTL format and range
///
/// Rules:
/// - Must be in format: {number}{unit} (e.g., "24h", "7d", "3600s")
/// - Valid units: s, m, h, d, w
/// - Minimum: 1 minute
/// - Maximum: 365 days
pub fn validate_ttl(ttl: Option<&str>) -> Result<Option<u64>, ValidationErrors> {
    let ttl = match ttl {
        None | Some("") => return Ok(None),
        Some(t) => t,
    };

    let mut errors = ValidationErrors::new();

    // Parse format like "24h", "7d", "3600s"
    let chars: Vec<char> = ttl.chars().collect();
    if chars.is_empty() {
        errors.add("ttl", "Invalid TTL format", ValidationRule::Format);
        return Err(errors);
    }

    // Find where digits end
    let num_end = chars.iter().position(|c| !c.is_ascii_digit()).unwrap_or(chars.len());
    if num_end == 0 {
        errors.add("ttl", "TTL must start with a number", ValidationRule::Format);
        return Err(errors);
    }

    let num: u64 = match ttl[..num_end].parse() {
        Ok(n) => n,
        Err(_) => {
            errors.add("ttl", "Invalid TTL number", ValidationRule::Format);
            return Err(errors);
        }
    };

    let unit = &ttl[num_end..];
    let seconds = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => num,
        "m" | "min" | "mins" | "minute" | "minutes" => num * 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => num * 3600,
        "d" | "day" | "days" => num * 86400,
        "w" | "week" | "weeks" => num * 604800,
        _ => {
            errors.add(
                "ttl",
                "Invalid TTL unit. Valid units: s, m, h, d, w",
                ValidationRule::Format,
            );
            return Err(errors);
        }
    };

    // Validate range (1 minute to 365 days)
    if seconds < 60 {
        errors.add(
            "ttl",
            "TTL must be at least 1 minute",
            ValidationRule::Range,
        );
    }

    if seconds > 365 * 86400 {
        errors.add(
            "ttl",
            "TTL must be at most 365 days",
            ValidationRule::Range,
        );
    }

    if errors.is_empty() {
        Ok(Some(seconds))
    } else {
        Err(errors)
    }
}

/// Validate file path (no path traversal)
///
/// Rules:
/// - Must not contain ".."
/// - Must be within allowed base directory
/// - Maximum 512 characters
pub fn validate_file_path(path: &str, base_dir: &Path) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();

    if path.is_empty() {
        errors.add("path", "Path is required", ValidationRule::Required);
        return Err(errors);
    }

    if path.len() > 512 {
        errors.add(
            "path",
            "Path must be 512 characters or less",
            ValidationRule::MaxLength,
        );
    }

    // Check for path traversal
    if path.contains("..") {
        errors.add(
            "path",
            "Path traversal (..) is not allowed",
            ValidationRule::PathTraversal,
        );
    }

    // Check if path is within base directory
    let full_path = Path::new(path);
    if let Ok(canonical) = full_path.canonicalize() {
        if let Ok(canonical_base) = base_dir.canonicalize() {
            if !canonical.starts_with(&canonical_base) {
                errors.add(
                    "path",
                    "Path must be within the allowed directory",
                    ValidationRule::PathTraversal,
                );
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate VM configuration
///
/// Rules:
/// - vCPUs: 1-128
/// - Memory: 64MB-1TB
pub fn validate_vm_config(vcpus: u8, memory_mb: u32) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();

    if vcpus < 1 {
        errors.add(
            "vcpus",
            "VM must have at least 1 vCPU",
            ValidationRule::Range,
        );
    }

    if vcpus > 128 {
        errors.add(
            "vcpus",
            "VM cannot have more than 128 vCPUs",
            ValidationRule::Range,
        );
    }

    if memory_mb < 64 {
        errors.add(
            "memory_mb",
            "VM must have at least 64MB of memory",
            ValidationRule::Range,
        );
    }

    if memory_mb > 1024 * 1024 {
        // 1TB
        errors.add(
            "memory_mb",
            "VM cannot have more than 1TB of memory",
            ValidationRule::Range,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate region name
///
/// Rules:
/// - Must be non-empty
/// - Maximum 64 characters
/// - Alphanumeric and hyphens only
pub fn validate_region(region: &str) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();

    if region.is_empty() {
        errors.add("region", "Region is required", ValidationRule::Required);
        return Err(errors);
    }

    if region.len() > 64 {
        errors.add(
            "region",
            "Region must be 64 characters or less",
            ValidationRule::MaxLength,
        );
    }

    if !region.chars().all(|c| c.is_alphanumeric() || c == '-') {
        errors.add(
            "region",
            "Region must contain only alphanumeric characters and hyphens",
            ValidationRule::Pattern,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate resume mode
///
/// Rules:
/// - Must be one of: collaborative, readonly, debug
pub fn validate_resume_mode(mode: &str) -> Result<(), ValidationErrors> {
    let valid_modes = ["collaborative", "readonly", "debug"];

    if !valid_modes.contains(&mode) {
        let mut errors = ValidationErrors::new();
        errors.add(
            "mode",
            &format!("Invalid mode. Valid modes: {}", valid_modes.join(", ")),
            ValidationRule::Pattern,
        );
        return Err(errors);
    }

    Ok(())
}

/// Validate all fields in a snapshot request
pub fn validate_snapshot_request(
    label: &str,
    ttl: Option<&str>,
) -> Result<(), ValidationErrors> {
    let mut all_errors = ValidationErrors::new();

    if let Err(mut e) = validate_label(label) {
        all_errors.errors.append(&mut e.errors);
    }

    if let Err(mut e) = validate_ttl(ttl) {
        all_errors.errors.append(&mut e.errors);
    }

    if all_errors.is_empty() {
        Ok(())
    } else {
        Err(all_errors)
    }
}

/// Validate all fields in a resume request
pub fn validate_resume_request(
    state_id: &str,
    mode: &str,
    region: Option<&str>,
) -> Result<(), ValidationErrors> {
    let mut all_errors = ValidationErrors::new();

    if let Err(mut e) = validate_state_id(state_id) {
        all_errors.errors.append(&mut e.errors);
    }

    if let Err(mut e) = validate_resume_mode(mode) {
        all_errors.errors.append(&mut e.errors);
    }

    if let Some(r) = region {
        if let Err(mut e) = validate_region(r) {
            all_errors.errors.append(&mut e.errors);
        }
    }

    if all_errors.is_empty() {
        Ok(())
    } else {
        Err(all_errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_state_id() {
        // Valid cases
        assert!(validate_state_id("abc123").is_ok());
        assert!(validate_state_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_state_id("test-state_123").is_ok());

        // Invalid cases
        assert!(validate_state_id("").is_err());
        assert!(validate_state_id("test@state").is_err());
        assert!(validate_state_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn test_validate_label() {
        // Valid cases
        assert!(validate_label("My State").is_ok());
        assert!(validate_label("production-database").is_ok());
        assert!(validate_label("test_state_123").is_ok());

        // Invalid cases
        assert!(validate_label("").is_err());
        assert!(validate_label("test@state").is_err());
        assert!(validate_label(&"a".repeat(257)).is_err());
    }

    #[test]
    fn test_validate_ttl() {
        // Valid cases
        assert_eq!(validate_ttl(Some("60s")).unwrap(), Some(60));
        assert_eq!(validate_ttl(Some("30m")).unwrap(), Some(1800));
        assert_eq!(validate_ttl(Some("24h")).unwrap(), Some(86400));
        assert_eq!(validate_ttl(Some("7d")).unwrap(), Some(604800));
        assert_eq!(validate_ttl(Some("1w")).unwrap(), Some(604800));
        assert_eq!(validate_ttl(None).unwrap(), None);
        assert_eq!(validate_ttl(Some("")).unwrap(), None);

        // Invalid cases
        assert!(validate_ttl(Some("10s")).is_err()); // Too short
        assert!(validate_ttl(Some("400d")).is_err()); // Too long
        assert!(validate_ttl(Some("invalid")).is_err());
        assert!(validate_ttl(Some("h24")).is_err()); // Wrong format
    }

    #[test]
    fn test_validate_vm_config() {
        // Valid cases
        assert!(validate_vm_config(1, 64).is_ok());
        assert!(validate_vm_config(4, 512).is_ok());
        assert!(validate_vm_config(128, 1024 * 1024).is_ok());

        // Invalid cases
        assert!(validate_vm_config(0, 512).is_err());
        assert!(validate_vm_config(129, 512).is_err());
        assert!(validate_vm_config(4, 32).is_err());
        assert!(validate_vm_config(4, 1024 * 1024 + 1).is_err());
    }

    #[test]
    fn test_validate_region() {
        // Valid cases
        assert!(validate_region("us-west-2").is_ok());
        assert!(validate_region("eu-central-1").is_ok());

        // Invalid cases
        assert!(validate_region("").is_err());
        assert!(validate_region("us_west_2").is_err()); // Underscores not allowed
        assert!(validate_region("us-west-2-east").is_err()); // Too long if > 64
    }

    #[test]
    fn test_validate_resume_mode() {
        // Valid cases
        assert!(validate_resume_mode("collaborative").is_ok());
        assert!(validate_resume_mode("readonly").is_ok());
        assert!(validate_resume_mode("debug").is_ok());

        // Invalid cases
        assert!(validate_resume_mode("admin").is_err());
        assert!(validate_resume_mode("write").is_err());
    }

    #[test]
    fn test_validation_errors_display() {
        let mut errors = ValidationErrors::new();
        errors.add("field1", "error 1", ValidationRule::Required);
        errors.add("field2", "error 2", ValidationRule::MaxLength);

        let display = format!("{}", errors);
        assert!(display.contains("field1"));
        assert!(display.contains("field2"));
    }
}
