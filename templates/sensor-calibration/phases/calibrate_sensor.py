import os
import sys
import time


def calibrate_sensor(phase, temperature_sensor, data_logger, run, log):
    slot_id = run.slot_id

    log.info("=" * 60)
    log.info("🔧 PHASE 1: SENSOR CALIBRATION")
    log.info("=" * 60)

    log.info(f"Slot ID: {slot_id}")

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
