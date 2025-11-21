#[cfg(test)]
mod tests {
    use crate::python::venv::*;
    use tempfile::TempDir;

    #[test]
    fn test_version_matches() {
        assert!(version_matches("3.11", "3.11"));
        assert!(version_matches("3.11.5", "3.11"));
        assert!(version_matches("3.11.5", "3.11.5"));
        assert!(!version_matches("3.10.5", "3.11"));
        assert!(!version_matches("3.11", "3.11.5"));
    }

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
    fn test_get_python_version_requirement_missing_file() {
        let temp = TempDir::new().unwrap();
        assert_eq!(get_python_version_requirement(temp.path()), None);
    }

    #[test]
    fn test_get_python_version_requirement_valid() {
        let temp = TempDir::new().unwrap();
        let toml = r#"
[project]
requires-python = ">=3.11"
"#;
        std::fs::write(temp.path().join("pyproject.toml"), toml).unwrap();
        assert_eq!(
            get_python_version_requirement(temp.path()),
            Some(">=3.11".to_string())
        );
    }

    #[test]
    fn test_create_manifest_file_exists() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("pyproject.toml"), "existing").unwrap();
        let result = create_manifest(temp.path());
        assert!(result.is_ok());
        let content = std::fs::read_to_string(temp.path().join("pyproject.toml")).unwrap();
        assert_eq!(content, "existing");
    }

    #[test]
    fn test_create_manifest_success() {
        let temp = TempDir::new().unwrap();
        create_manifest(temp.path()).unwrap();
        assert!(temp.path().join("pyproject.toml").exists());
        let content = std::fs::read_to_string(temp.path().join("pyproject.toml")).unwrap();
        assert!(content.contains("requires-python"));
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
}
