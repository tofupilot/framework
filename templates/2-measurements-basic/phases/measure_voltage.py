import random


def measure_voltage(measurements, log):
    voltage = round(random.uniform(4.7, 5.3), 2)

    log.info(f"Measuring voltage: {voltage}V")
    measurements.output_voltage = voltage

    log.info("Voltage measurement complete")
