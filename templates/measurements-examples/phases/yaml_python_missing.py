def test_missing_values_outcomes(phase, test_api, ui):
    """YAML declares structure, Python may provide values"""

    test_api.measurements.missing_measurement = 2.5

    test_api.log.info("Missing measurement: YAML defines structure")
    test_api.log.info("Python provides measurement value")

    
