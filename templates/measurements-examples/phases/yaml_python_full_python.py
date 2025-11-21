import random

def test_full_python(phase, test_api, ui):
    """Correct pattern: YAML defines specs, Python provides values"""

    current = random.uniform(0.08, 0.15)
    test_api.measurements.python_only_current = current

    voltage = 3.3
    power = voltage * current
    test_api.measurements.calculated_power = power

    temperature = random.uniform(18, 35)
    test_api.measurements.python_only_temperature = temperature

    test_api.log.info(f"Current: {current*1000:.1f}mA")
    test_api.log.info(f"Power: {power*1000:.1f}mW")
    test_api.log.info("✅ BEST PRACTICE: Define specs in YAML, provide values in Python")

    
