import sys
import os
import random
import statistics
def test_auto_evaluation(phase, test_api, ui):
    """Measurement with validator and aggregation - YAML defines all specs, Python provides values only"""

    # Generate multiple voltage readings for aggregation calculation
    voltage_readings = [3.2 + random.gauss(0, 0.05) for _ in range(50)]
    primary_voltage = voltage_readings[0]

    # Calculate mean for aggregation (but don't mention the validator on this aggregation)
    mean_voltage = statistics.mean(voltage_readings)

    # ✅ Correct: YAML defines all specs, Python provides computed aggregation value
    # The validator (>= 3.0) and aggregation validator (> 3.1) are defined in YAML only
    aggregation_value = [
        Aggregation(aggregation_type='mean', value=mean_voltage)  # YAML defines type/unit, Python provides value
    ]

    test_api.measurements.auto_eval_measurement = primary_voltage, aggregations=aggregation_value

    test_api.log.info(f"Auto-evaluation measurement: {primary_voltage:.3f}V")
    test_api.log.info(f"Mean aggregation: {mean_voltage:.3f}V")
    test_api.log.info("✅ Validators and aggregation specs defined in YAML, auto-evaluated by system")
    test_api.log.info("✅ Python provides only measurement value and computed aggregation value")

    