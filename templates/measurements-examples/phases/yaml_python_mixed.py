import random
import statistics

def test_mixed_examples(phase, run, ui):
    """Mixed validator and aggregation approaches"""

    voltage = 3.1 + random.uniform(-0.2, 0.2)
    measurements.mixed_validators_measurement = voltage

    current_readings = [0.12 + random.gauss(0, 0.01) for _ in range(30)]
    primary_current = current_readings[0]
    measurements.mixed_aggregations_measurement = primary_current

    run.log.info(f"Mixed validators voltage: {voltage:.3f}V")
    run.log.info(f"Mixed aggregations current: {primary_current:.3f}A")

    
