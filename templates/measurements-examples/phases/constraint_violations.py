def test_constraint_violations(phase, test_api, ui):
    """Test cases - constraints are now enforced by Rust/YAML layer"""

    test_api.log.info("All constraints enforced by YAML configuration")

    test_api.measurements.unit_violation = 3.3
    test_api.measurements.docstring_violation = 25.0
    test_api.measurements.validator_spec_violation = 3.1
    test_api.measurements.aggregation_spec_violation = 3.2

    test_api.log.info("✅ All specifications defined in YAML")

    
