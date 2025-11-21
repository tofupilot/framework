import sys
import os
import random
def test_maximal_empty_null(phase, test_api, ui):
    """Missing sensor with null validator, availability aggregations, and count validator"""

    # Simulate sensor readings over time - some may be null due to sensor failures
    sensor_readings = []
    for _ in range(100):
        if random.random() < 0.15:  # 15% chance of sensor failure
            sensor_readings.append(None)
        else:
            sensor_readings.append(random.uniform(20.0, 25.0))

    # Calculate availability aggregations
    null_count = sum(1 for reading in sensor_readings if reading is None)
    total_count = len(sensor_readings)
    availability = ((total_count - null_count) / total_count) * 100
    status = "DEGRADED" if null_count > 0 else "OPERATIONAL"

    # Primary sensor reading (first reading, which may be null)
    primary_sensor_reading = sensor_readings[0]

    test_api.measurements.missing_sensor = primary_sensor_reading

    test_api.log.info(f"Primary sensor reading: {primary_sensor_reading}")
    test_api.log.info(f"Null readings: {null_count}/{total_count}")
    test_api.log.info(f"Availability: {availability:.1f}%")
    test_api.log.info(f"System status: {status}")

    