use crate::export::control_mapping::{ControlMapping, ControlSelector};
use crate::export::soc2::generate_soc2_export_with_options;
use crate::state::layout::Layout;
use miette::{Result, miette};
use std::path::Path;

pub fn generate_soc2_control_export(
    layout: &Layout,
    demo: bool,
    keys_dir: Option<&Path>,
    controls: &[String],
) -> Result<Vec<u8>> {
    if controls.is_empty() {
        return Err(miette!("at least one --control value is required"));
    }
    let selector = ControlSelector::new(controls.to_vec());
    let mapping = ControlMapping::load_static()?;
    let _selected = selector.select(&mapping)?;
    generate_soc2_export_with_options(layout, demo, keys_dir, Some(&selector))
}
