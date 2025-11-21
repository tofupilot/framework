import os
import sys
import time


def calibrate_sensor(phase, log, run, temperature_sensor, data_logger):
    log.info("=" * 60)
    log.info("🔧 PHASE 1: SENSOR CALIBRATION")
    log.info("=" * 60)

    log.info(f"Slot ID: {run.slot_id}")

    temperature_sensor.connect()
    sensor_info = temperature_sensor.get_info()
    log.info(f"Sensor Info: {sensor_info}")

    log.info("Taking reference measurements at known temperatures...")
    time.sleep(0.5)

    reference_temps = [0.0, 25.0, 100.0]
    measurements = []

    for ref_temp in reference_temps:
        log.info(f"Reference point: {ref_temp}°C")
        time.sleep(0.3)

        measured = temperature_sensor.read_temperature()
        measurements.append((ref_temp, measured))

        data_logger.log_measurement(sensor_info["id"], measured, "calibration")

    errors = [measured - ref for ref, measured in measurements]
    avg_error = sum(errors) / len(errors)

    log.info("Calibration Analysis:")
    log.info(f"  Average error: {avg_error:.2f}°C")
    log.info(f"  Setting calibration offset: {-avg_error:.2f}°C")

    temperature_sensor.set_calibration_offset(-avg_error)

    log.info("Calibration complete!")
    log.info("=" * 60)

    


def validate_calibration(phase, log, run, temperature_sensor, data_logger):
    log.info("=" * 60)
    log.info("✓ PHASE 2: CALIBRATION VALIDATION")
    log.info("=" * 60)

    log.info(f"Slot ID: {run.slot_id}")

    sensor_info = temperature_sensor.get_info()
    log.info(f"Sensor calibration offset: {sensor_info['calibration_offset']}°C")

    log.info("Validating calibration with test measurements...")
    time.sleep(0.5)

    validation_temps = []
    for i in range(5):
        temp = temperature_sensor.read_temperature()
        validation_temps.append(temp)
        data_logger.log_measurement(sensor_info["id"], temp, "validation")
        time.sleep(0.2)

    avg_temp = sum(validation_temps) / len(validation_temps)
    temp_range = max(validation_temps) - min(validation_temps)

    log.info("Validation Results:")
    log.info(f"  Average temperature: {avg_temp:.2f}°C")
    log.info(f"  Temperature range: {temp_range:.2f}°C")

    stats = data_logger.get_statistics()
    log.info("Overall Session Statistics:")
    log.info(f"  Total measurements: {stats['count']}")
    log.info(
        f"  Min: {stats['min']}°C | Max: {stats['max']}°C | Avg: {stats['avg']}°C"
    )

    if temp_range < 1.0:
        log.info("Calibration validated successfully! Sensor readings are stable.")
    else:
        log.warning("Temperature range exceeds tolerance (1.0°C)")
        phase.fail(f"Temperature range {temp_range:.2f}°C exceeds tolerance (1.0°C)")

    log.info("=" * 60)
