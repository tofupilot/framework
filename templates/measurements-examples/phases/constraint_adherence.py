import random
import statistics

def test_constraint_adherence(phase, run, ui):
    """Proper constraint adherence - values only"""

    run.log.info("Testing proper constraint adherence:")

    voltage = 3.3 + random.uniform(-0.1, 0.1)
    measurements.values_only_measurement = voltage
    run.log.info(f"✅ Values only: {voltage:.3f}V")

    current = 0.12 + random.uniform(-0.02, 0.02)
    measurements.outcome_matching_measurement = current
    run.log.info(f"✅ Outcome matching: {current:.3f}A")

    readings = [3.1 + random.gauss(0, 0.05) for _ in range(20)]
    primary_reading = readings[0]
    measurements.aggregation_values_measurement = primary_reading
    run.log.info(f"✅ Aggregation values: {primary_reading:.3f}V")

    run.log.info("✅ All specifications in YAML, values in Python")

    
