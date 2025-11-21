def inspect_finger_motors(phase, run, ethercat):
    ethercat.start_motor_oscillation("fingers", 1.5, 6.0)
    ethercat.stop_motor_oscillation("fingers")
    
