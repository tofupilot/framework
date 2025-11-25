import random


def measure_temperature(measurements, log):
    temperature = round(random.uniform(18.0, 32.0), 1)

    log.info(f"Measuring temperature: {temperature}°C")
    measurements.ambient_temperature = temperature

    log.info("Temperature measurement complete")
