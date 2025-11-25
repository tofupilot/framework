import time
import random

def current_test(log, measurements):
    log.info("Starting current test...")
    time.sleep(1.5)

    current = round(random.uniform(100, 450), 1)
    measurements.supply_current = current

    log.info(f"Measured current: {current}mA")
