use std::path::PathBuf;

pub(crate) fn find_venv_path(project_path: &std::path::Path) -> Option<PathBuf> {
    let venv_candidates = vec![
        project_path.join(".venv"),
        project_path.join("venv"),
    ];

    for venv_path in venv_candidates {
        if venv_path.exists() && venv_path.is_dir() {
            log::debug!("Found virtual environment at: {}", venv_path.display());
            return Some(venv_path);
        }
    }

    log::debug!("No virtual environment found in: {}", project_path.display());
    None
}

pub(crate) fn get_venv_python_path(venv_path: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    }
}
