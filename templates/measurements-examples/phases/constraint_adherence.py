import random
import statistics

def test_constraint_adherence(phase, test_api, ui):
    """Proper constraint adherence - values only"""

    test_api.log.info("Testing proper constraint adherence:")

    voltage = 3.3 + random.uniform(-0.1, 0.1)
    test_api.measurements.values_only_measurement = voltage
    test_api.log.info(f"✅ Values only: {voltage:.3f}V")

    current = 0.12 + random.uniform(-0.02, 0.02)
    test_api.measurements.outcome_matching_measurement = current
    test_api.log.info(f"✅ Outcome matching: {current:.3f}A")

    readings = [3.1 + random.gauss(0, 0.05) for _ in range(20)]
    primary_reading = readings[0]
    test_api.measurements.aggregation_values_measurement = primary_reading
    test_api.log.info(f"✅ Aggregation values: {primary_reading:.3f}V")

    test_api.log.info("✅ All specifications in YAML, values in Python")

    
