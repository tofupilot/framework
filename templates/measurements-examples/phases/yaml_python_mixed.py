import random
import statistics

def test_mixed_examples(phase, test_api, ui):
    """Mixed validator and aggregation approaches"""

    voltage = 3.1 + random.uniform(-0.2, 0.2)
    test_api.measurements.mixed_validators_measurement = voltage

    current_readings = [0.12 + random.gauss(0, 0.01) for _ in range(30)]
    primary_current = current_readings[0]
    test_api.measurements.mixed_aggregations_measurement = primary_current

    test_api.log.info(f"Mixed validators voltage: {voltage:.3f}V")
    test_api.log.info(f"Mixed aggregations current: {primary_current:.3f}A")

    
