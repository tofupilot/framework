import time
import numpy as np


def print_information_in_phase():
    print("Waiting 2 sec:")
    time.sleep(2)
    print("Just waited 2 sec!")


def import_lib(measurements):
    a = np.arange(15).reshape(3, 5)
    measurements.array = a
