def test_missing_values_outcomes(phase, run, ui):
    """YAML declares structure, Python may provide values"""

    measurements.missing_measurement = 2.5

    run.log.info("Missing measurement: YAML defines structure")
    run.log.info("Python provides measurement value")

    
