use super::*;
use crate::procedure::schema::PhaseDefinition;

fn create_test_phase(key: &str, name: &str, depends_on: Vec<String>) -> PhaseDefinition {
    serde_json::from_value(serde_json::json!({
        "key": key,
        "name": name,
        "depends_on": depends_on,
    })).unwrap()
}

#[test]
fn test_detect_circular_dependencies_direct() {
    let phases = vec![
        create_test_phase("a", "A", vec!["b".to_string()]),
        create_test_phase("b", "B", vec!["a".to_string()]),
    ];

    let cycle = detect_circular_dependencies(&phases);
    assert!(cycle.is_some());
    let cycle = cycle.unwrap();
    assert!(cycle.contains(&"a".to_string()));
    assert!(cycle.contains(&"b".to_string()));
}

#[test]
fn test_circular_dependencies_three_nodes() {
    let phases = vec![
        create_test_phase("a", "A", vec!["b".to_string()]),
        create_test_phase("b", "B", vec!["c".to_string()]),
        create_test_phase("c", "C", vec!["a".to_string()]),
    ];

    let cycle = detect_circular_dependencies(&phases);
    assert!(cycle.is_some());
}

#[test]
fn test_no_circular_dependencies() {
    let phases = vec![
        create_test_phase("a", "A", vec![]),
        create_test_phase("b", "B", vec!["a".to_string()]),
        create_test_phase("c", "C", vec!["b".to_string()]),
    ];

    let cycle = detect_circular_dependencies(&phases);
    assert!(cycle.is_none());
}

#[test]
fn test_no_dependencies() {
    let phases = vec![
        create_test_phase("a", "A", vec![]),
        create_test_phase("b", "B", vec![]),
        create_test_phase("c", "C", vec![]),
    ];

    let cycle = detect_circular_dependencies(&phases);
    assert!(cycle.is_none());
}

#[test]
fn test_self_dependency_is_cycle() {
    let phases = vec![
        create_test_phase("a", "A", vec!["a".to_string()]),
    ];

    let cycle = detect_circular_dependencies(&phases);
    assert!(cycle.is_some());
}
