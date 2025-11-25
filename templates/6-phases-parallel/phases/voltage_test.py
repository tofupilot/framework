import time
import random

def voltage_test(log, measurements):
    log.info("Starting voltage test...")
    time.sleep(1)

    voltage = round(random.uniform(4.9, 5.1), 2)
    measurements.supply_voltage = voltage

    log.info(f"Measured voltage: {voltage}V")
