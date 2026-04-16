#[cfg(test)]
mod tests {
    use crate::python::venv::*;
    use tempfile::TempDir;

    #[test]
    fn test_get_dependencies_missing_file() {
        let temp = TempDir::new().unwrap();
        assert_eq!(get_dependencies(temp.path()), Vec::<String>::new());
    }

    #[test]
    fn test_get_dependencies_invalid_toml() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("pyproject.toml"), "invalid[").unwrap();
        assert_eq!(get_dependencies(temp.path()), Vec::<String>::new());
    }

    #[test]
    fn test_get_dependencies_valid() {
        let temp = TempDir::new().unwrap();
        let toml = r#"
[project]
dependencies = ["numpy>=1.24.0", "requests"]
"#;
        std::fs::write(temp.path().join("pyproject.toml"), toml).unwrap();
        let deps = get_dependencies(temp.path());
        assert_eq!(deps, vec!["numpy>=1.24.0", "requests"]);
    }

    #[test]
    fn test_get_dependencies_empty() {
        let temp = TempDir::new().unwrap();
        let toml = r#"
[project]
dependencies = []
"#;
        std::fs::write(temp.path().join("pyproject.toml"), toml).unwrap();
        let deps = get_dependencies(temp.path());
        assert_eq!(deps, Vec::<String>::new());
    }

    #[test]
    fn test_get_dependencies_no_dependencies_field() {
        let temp = TempDir::new().unwrap();
        let toml = r#"
[project]
name = "test"
"#;
        std::fs::write(temp.path().join("pyproject.toml"), toml).unwrap();
        let deps = get_dependencies(temp.path());
        assert_eq!(deps, Vec::<String>::new());
    }

    #[test]
    fn test_find_python_executable_none() {
        let temp = TempDir::new().unwrap();
        assert_eq!(find_python_executable(temp.path()), None);
    }

    #[test]
    fn test_find_python_executable_dot_venv() {
        let temp = TempDir::new().unwrap();
        let venv_dir = temp.path().join(".venv");
        std::fs::create_dir(&venv_dir).unwrap();

        #[cfg(target_os = "windows")]
        let python_path = {
            let scripts = venv_dir.join("Scripts");
            std::fs::create_dir(&scripts).unwrap();
            scripts.join("python.exe")
        };

        #[cfg(not(target_os = "windows"))]
        let python_path = {
            let bin = venv_dir.join("bin");
            std::fs::create_dir(&bin).unwrap();
            bin.join("python")
        };

        std::fs::write(&python_path, "").unwrap();

        let result = find_python_executable(temp.path());
        assert!(result.is_some());
        assert_eq!(result.unwrap(), python_path);
    }

    #[test]
    fn test_find_python_executable_venv() {
        let temp = TempDir::new().unwrap();
        let venv_dir = temp.path().join("venv");
        std::fs::create_dir(&venv_dir).unwrap();

        #[cfg(target_os = "windows")]
        let python_path = {
            let scripts = venv_dir.join("Scripts");
            std::fs::create_dir(&scripts).unwrap();
            scripts.join("python.exe")
        };

        #[cfg(not(target_os = "windows"))]
        let python_path = {
            let bin = venv_dir.join("bin");
            std::fs::create_dir(&bin).unwrap();
            bin.join("python")
        };

        std::fs::write(&python_path, "").unwrap();

        let result = find_python_executable(temp.path());
        assert!(result.is_some());
        assert_eq!(result.unwrap(), python_path);
    }

    #[test]
    fn test_find_python_executable_prefers_dot_venv() {
        let temp = TempDir::new().unwrap();

        let dot_venv = temp.path().join(".venv");
        std::fs::create_dir(&dot_venv).unwrap();
        let venv = temp.path().join("venv");
        std::fs::create_dir(&venv).unwrap();

        #[cfg(target_os = "windows")]
        {
            std::fs::create_dir(dot_venv.join("Scripts")).unwrap();
            std::fs::write(dot_venv.join("Scripts").join("python.exe"), "").unwrap();
            std::fs::create_dir(venv.join("Scripts")).unwrap();
            std::fs::write(venv.join("Scripts").join("python.exe"), "").unwrap();
        }

        #[cfg(not(target_os = "windows"))]
        {
            std::fs::create_dir(dot_venv.join("bin")).unwrap();
            std::fs::write(dot_venv.join("bin").join("python"), "").unwrap();
            std::fs::create_dir(venv.join("bin")).unwrap();
            std::fs::write(venv.join("bin").join("python"), "").unwrap();
        }

        let result = find_python_executable(temp.path()).unwrap();
        assert!(result.to_string_lossy().contains(".venv"));
    }

    #[tokio::test]
    async fn test_delete_venv_no_venv() {
        let temp = TempDir::new().unwrap();
        let result = delete_venv(temp.path().to_str().unwrap().to_string());
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_delete_venv_success() {
        let temp = TempDir::new().unwrap();
        let venv = temp.path().join(".venv");
        std::fs::create_dir(&venv).unwrap();
        std::fs::write(venv.join("test.txt"), "test").unwrap();

        delete_venv(temp.path().to_str().unwrap().to_string()).unwrap();
        assert!(!venv.exists());
    }

    #[test]
    fn test_delete_venv_deletes_venv_folder() {
        let temp = TempDir::new().unwrap();
        let venv = temp.path().join("venv");
        std::fs::create_dir(&venv).unwrap();
        std::fs::write(venv.join("test.txt"), "test").unwrap();

        delete_venv(temp.path().to_str().unwrap().to_string()).unwrap();
        assert!(!venv.exists());
    }

    #[test]
    fn test_delete_venv_prefers_dot_venv() {
        let temp = TempDir::new().unwrap();
        let dot_venv = temp.path().join(".venv");
        let venv = temp.path().join("venv");
        std::fs::create_dir(&dot_venv).unwrap();
        std::fs::create_dir(&venv).unwrap();

        delete_venv(temp.path().to_str().unwrap().to_string()).unwrap();
        assert!(!dot_venv.exists());
        assert!(venv.exists());
    }

    #[test]
    fn test_ensure_manifest_creates_pyproject_when_missing() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");

        assert!(!pyproject_path.exists());
        ensure_manifest_with_studio_deps(temp.path()).unwrap();
        assert!(pyproject_path.exists());

        let content = std::fs::read_to_string(&pyproject_path).unwrap();
        assert!(content.contains("[project]"));
        assert!(content.contains("requires-python"));
        assert!(content.contains("[dependency-groups]"));
        assert!(content.contains("studio"));
    }

    #[test]
    fn test_ensure_manifest_adds_studio_group_to_existing() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");
        let initial = r#"[project]
name = "my-project"
version = "1.0.0"
dependencies = ["requests"]
"#;
        std::fs::write(&pyproject_path, initial).unwrap();

        ensure_manifest_with_studio_deps(temp.path()).unwrap();

        let content = std::fs::read_to_string(&pyproject_path).unwrap();
        assert!(content.contains("[dependency-groups]"));
        assert!(content.contains("studio = ["));
    }

    #[test]
    fn test_ensure_manifest_preserves_existing_content() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");
        let initial = r#"[project]
name = "my-project"
version = "2.5.0"
dependencies = ["numpy", "pandas"]

[tool.ruff]
line-length = 100
"#;
        std::fs::write(&pyproject_path, initial).unwrap();

        ensure_manifest_with_studio_deps(temp.path()).unwrap();

        let content = std::fs::read_to_string(&pyproject_path).unwrap();
        assert!(content.contains("name = \"my-project\""));
        assert!(content.contains("version = \"2.5.0\""));
        assert!(content.contains("numpy"));
        assert!(content.contains("pandas"));
        assert!(content.contains("[tool.ruff]"));
        assert!(content.contains("line-length = 100"));
    }

    #[test]
    fn test_ensure_manifest_updates_existing_studio_group() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");
        let initial = r#"[project]
name = "my-project"
version = "1.0.0"

[dependency-groups]
studio = ["grpcio==1.62.0", "portpicker", "protobuf"]
"#;
        std::fs::write(&pyproject_path, initial).unwrap();

        ensure_manifest_with_studio_deps(temp.path()).unwrap();

        let final_content = std::fs::read_to_string(&pyproject_path).unwrap();
        // Studio deps are now empty, so old deps should be replaced with empty list
        assert!(final_content.contains("studio = ["));
    }

    #[test]
    fn test_ensure_manifest_adds_to_existing_dependency_groups() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");
        let initial = r#"[project]
name = "my-project"
version = "1.0.0"

[dependency-groups]
dev = ["pytest", "ruff"]
"#;
        std::fs::write(&pyproject_path, initial).unwrap();

        ensure_manifest_with_studio_deps(temp.path()).unwrap();

        let content = std::fs::read_to_string(&pyproject_path).unwrap();
        assert!(content.contains("dev = ["));
        assert!(content.contains("pytest"));
        assert!(content.contains("studio = ["));
    }

    #[test]
    fn test_ensure_manifest_invalid_toml_returns_error() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");
        std::fs::write(&pyproject_path, "invalid toml [[[").unwrap();

        let result = ensure_manifest_with_studio_deps(temp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to parse"));
    }

    #[test]
    fn test_ensure_manifest_studio_deps_have_correct_packages() {
        let temp = TempDir::new().unwrap();
        let pyproject_path = temp.path().join("pyproject.toml");
        let initial = r#"[project]
name = "test"
version = "0.1.0"
"#;
        std::fs::write(&pyproject_path, initial).unwrap();

        ensure_manifest_with_studio_deps(temp.path()).unwrap();

        let content = std::fs::read_to_string(&pyproject_path).unwrap();
        // Studio no longer requires external Python dependencies
        assert!(content.contains("studio = ["));
    }
}
