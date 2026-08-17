use crate::cli::args::{FederateCommands, GateCommands, PolicyCommands, ServiceSubcommands};
use miette::Result;

pub(super) fn dispatch_federate(command: FederateCommands) -> Result<()> {
    match command {
        FederateCommands::Export { dry_run, out } => {
            crate::commands::federate::execute_federate_export(dry_run, out)
        }
        FederateCommands::Scan => crate::commands::federate::execute_federate_scan(),
        FederateCommands::Status => crate::commands::federate::execute_federate_status(),
    }
}
pub(super) fn dispatch_services(
    command: ServiceSubcommands,
    config: &crate::config::model::Config,
) -> Result<()> {
    match command {
        ServiceSubcommands::Diff(args) => {
            crate::commands::services_diff::execute_services_diff(args, config)
        }
    }
}
/// Bare `gate` → show current mode only (`Mode { mode: None }`); never invent set.
pub(super) fn gate_or_default(command: Option<GateCommands>) -> GateCommands {
    command.unwrap_or(GateCommands::Mode { mode: None })
}

/// Bare `policy` → `check` with text defaults.
pub(super) fn policy_or_default(command: Option<PolicyCommands>) -> PolicyCommands {
    command.unwrap_or(PolicyCommands::Check {
        pr: None,
        fail_on: None,
        policy: None,
        format: None,
    })
}

/// Bare `federate` → `status` (read-only). Never default to Export (writes).
pub(super) fn federate_or_default(command: Option<FederateCommands>) -> FederateCommands {
    command.unwrap_or(FederateCommands::Status)
}

/// Bare `services` → `diff` with default flags (soft optional 0179).
pub(super) fn services_or_default(command: Option<ServiceSubcommands>) -> ServiceSubcommands {
    command.unwrap_or(ServiceSubcommands::Diff(
        crate::commands::services_diff::ServicesDiffArgs::default(),
    ))
}

pub(super) fn dispatch_gate(command: GateCommands) -> Result<()> {
    match command {
        GateCommands::Mode { mode } => {
            let layout = crate::commands::helpers::get_layout()?;
            if let Some(mode) = mode {
                let mode = mode.to_lowercase();
                if !crate::config::model::GateConfig::valid_modes().contains(&mode.as_str()) {
                    return Err(miette::miette!(
                        "invalid gate mode '{}'; valid modes are: observe, enforce",
                        mode
                    ));
                }
                let config = crate::config::load::load_config(&layout).unwrap_or_default();
                let old_mode = config.gate.mode.clone();
                if old_mode == mode {
                    println!("Gate mode is already: {}", mode);
                    return Ok(());
                }
                crate::commands::gate::write_mode_transition_entry(&layout, &old_mode, &mode)?;
                crate::commands::config::execute_config_set_in(
                    &layout,
                    &format!("gate.mode={}", mode),
                )?;
                println!("Gate mode changed: {} → {}", old_mode, mode);
            } else {
                let config = crate::config::load::load_config(&layout).unwrap_or_default();
                println!("Gate mode: {}", config.gate.mode);
            }
            Ok(())
        }
    }
}

pub(super) fn dispatch_policy(command: PolicyCommands) -> Result<()> {
    match command {
        PolicyCommands::Check {
            pr,
            fail_on,
            policy,
            format,
        } => crate::commands::policy_check::execute_policy_check(pr, fail_on, policy, format),
    }
}
