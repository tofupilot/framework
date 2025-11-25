import random

def test_full_python(phase, run, ui):
    """Correct pattern: YAML defines specs, Python provides values"""

    current = random.uniform(0.08, 0.15)
    measurements.python_only_current = current

    voltage = 3.3
    power = voltage * current
    measurements.calculated_power = power

    temperature = random.uniform(18, 35)
    measurements.python_only_temperature = temperature

    run.log.info(f"Current: {current*1000:.1f}mA")
    run.log.info(f"Power: {power*1000:.1f}mW")
    run.log.info("✅ BEST PRACTICE: Define specs in YAML, provide values in Python")

    
