mod capability;
mod catalog;
mod custom;
mod lifecycle;
mod mcp;
mod skill;
mod supervisor;

pub use capability::{
    BUILTIN_NAMESPACE, BUILTIN_VERSION, CapabilityDescriptor, CapabilityEffect, CapabilitySource,
    CapabilitySummary, Provenance, RecoveryMode, capability_id, hash_bytes, hash_schema,
    parse_capability_id,
};
pub use catalog::ExtensionCatalog;
pub use custom::{CustomJsonTool, CustomToolManifest, load_custom_manifest};
pub use lifecycle::{ExtensionHealth, ExtensionState, ExtensionSummary};
pub use mcp::{
    McpHttpTool, McpInstallSpec, McpListedTool, McpStdioClient, McpTool, enforce_workspace_ceiling,
    mcp_http_call, mcp_http_initialize, mcp_http_list_tools, validate_mcp_http_url,
};
pub use skill::{Skill, SkillCatalog, SkillMeta, SkillSummary, load_skill, parse_skill};
pub use supervisor::{
    ChildSupervisor, SpawnSpec, SupervisorOutput, is_blocked_env, validate_command, validate_path,
};
