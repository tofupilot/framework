import time
import random

def temperature_test(log, measurements):
    log.info("Starting temperature test...")
    time.sleep(1.2)

    temperature = round(random.uniform(30, 42), 1)
    measurements.board_temperature = temperature

    log.info(f"Measured temperature: {temperature}°C")
