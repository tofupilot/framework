import time


def perform_electrical_tests(phase, measurements):
    print("Waiting 2 sec.....")
    # time.sleep(2)
    print("Get voltage: ")
    measurements.voltage = 4
    time.sleep(3)


def get_temperature(phase, measurements):
    print("Getting voltage info....")
    measurements.temperature = 30
    measurements.input_temperature = 35
    measurements.temperature_with_units = 3
    time.sleep(1)


def get_data(phase, measurements):
    measurements.measure_one = 1
    measurements.measure_two = "hello"
    measurements.measure_three = True
    measurements.measure_four = "OK"
    measurements.measure_five = [1, 2, 3, 4, 5]
    measurements.measure_six = 6
    measurements.double_validators = 30
    measurements.empty = None
    time.sleep(1)


def init_test(phase, measurements):
    measurements.power_on = "on"
    print("Let's go")


def execute_teardown(phase, measurements):
    print("Ending the test....")
    measurements.power_off = "off"
    print("FINISH")


def phase_only(phase):
    print("doing stuff here")
    # time.sleep(2)
      # phase.stop() >> to check dependencies
