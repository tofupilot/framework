import sys
import os
import random
import statistics
def test_maximal_numeric(phase, test_api, ui):
    """Voltage with range validators, statistical aggregations - YAML defines specs, Python provides values"""

    # Generate multiple voltage readings for statistical analysis
    voltage_readings = [3.3 + random.gauss(0, 0.05) for _ in range(100)]

    # Calculate aggregations
    mean_voltage = statistics.mean(voltage_readings)
    std_dev = statistics.stdev(voltage_readings)
    voltage_range = max(voltage_readings) - min(voltage_readings)

    # Main voltage measurement - YAML defines all specs, Python provides values/outcomes
    primary_voltage = voltage_readings[0]

    # ✅ Correct: YAML defines aggregation types/units, Python provides computed values
    aggregation_values = [
        Aggregation(aggregation_type='mean', value=mean_voltage, outcome="PASS",
                   validators=[
                       # Outcome for YAML-defined aggregation validator
                       Validator(level="alert", operator=">", outcome="PASS")
                   ]),
        Aggregation(aggregation_type='std_dev', value=std_dev, outcome="PASS"),
        Aggregation(aggregation_type='range', value=voltage_range, outcome="PASS")
    ]

    test_api.measurements.maximal_voltage = primary_voltage, aggregations=aggregation_values

    test_api.log.info(f"Primary voltage: {primary_voltage:.4f}V")
    test_api.log.info(f"Mean voltage: {mean_voltage:.4f}V")
    test_api.log.info(f"Standard deviation: {std_dev:.4f}V")
    test_api.log.info(f"Range: {voltage_range:.4f}V")

    