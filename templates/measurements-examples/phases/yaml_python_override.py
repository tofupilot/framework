import random
import statistics

def test_override_outcomes(phase, run, ui):
    """YAML defines specifications, Python provides values"""

    voltage_readings = [3.0 + random.gauss(0, 0.1) for _ in range(50)]
    primary_voltage = voltage_readings[0]
    measurements.override_measurement = primary_voltage

    run.log.info(f"Override measurement: {primary_voltage:.3f}V")
    run.log.info("YAML has specifications, Python provides values")

    
