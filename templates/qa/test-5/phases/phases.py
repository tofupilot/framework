import random
import time


def get_temperature(measurements):
    print("I'm here... getting temperature")
    time.sleep(2)
    print("I just waited 2 sec")
    measurements.temperature = 26
    print("I just got the temperature")


def get_input_voltage(measurements):
    print("I'm here... getting voltage")
    time.sleep(2)
    print("I just waited 2 sec")
    print("Voltage ....")
    measurements.input_voltage = 3.3
