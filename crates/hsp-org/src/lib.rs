use std::path::{Path, PathBuf};

pub const PY_REFERENCE_DIR: &str = "references/hsp-py";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HspWorkspace {
    pub root: PathBuf,
    pub workgroups: Vec<orgmap::WorkgroupDefinition>,
    pub py_reference: PathBuf,
}

impl HspWorkspace {
    pub fn discover(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let workgroups = orgmap::discover_workgroup_stack(path);
        let root = workgroups
            .last()
            .map(|workgroup| workgroup.root.clone())
            .unwrap_or_else(|| path.to_path_buf());

        Self {
            root,
            workgroups,
            py_reference: PathBuf::from(PY_REFERENCE_DIR),
        }
    }

    pub fn active_workgroup_name(&self) -> Option<&str> {
        self.workgroups
            .last()
            .map(|workgroup| workgroup.name.as_str())
    }
}
