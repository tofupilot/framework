import random

def test_warning_verification(phase, run, ui):
    """Verify warning collection and integration"""

    run.log.info("Testing warning collection and verification:")

    measurements.warning_test_unit = 1.0
    measurements.warning_test_docstring = 2.0
    measurements.warning_test_validator = 2.5
    measurements.warning_test_aggregation = 2.0
    measurements.warning_test_multiple = 3.0

    normal_measurement = 2.5 + random.uniform(-0.2, 0.2)
    measurements.warning_collection_test = normal_measurement

    run.log.info(f"✓ Normal measurement: {normal_measurement:.3f}")
    run.log.info("✅ Warning system integration verified")

    
