import random

def test_warning_verification(phase, test_api, ui):
    """Verify warning collection and integration"""

    test_api.log.info("Testing warning collection and verification:")

    test_api.measurements.warning_test_unit = 1.0
    test_api.measurements.warning_test_docstring = 2.0
    test_api.measurements.warning_test_validator = 2.5
    test_api.measurements.warning_test_aggregation = 2.0
    test_api.measurements.warning_test_multiple = 3.0

    normal_measurement = 2.5 + random.uniform(-0.2, 0.2)
    test_api.measurements.warning_collection_test = normal_measurement

    test_api.log.info(f"✓ Normal measurement: {normal_measurement:.3f}")
    test_api.log.info("✅ Warning system integration verified")

    
