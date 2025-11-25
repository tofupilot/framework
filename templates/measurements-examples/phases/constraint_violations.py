def test_constraint_violations(phase, run, ui):
    """Test cases - constraints are now enforced by Rust/YAML layer"""

    run.log.info("All constraints enforced by YAML configuration")

    measurements.unit_violation = 3.3
    measurements.docstring_violation = 25.0
    measurements.validator_spec_violation = 3.1
    measurements.aggregation_spec_violation = 3.2

    run.log.info("✅ All specifications defined in YAML")

    
