def inspect_thumb_motor(phase, run, ethercat):
    ethercat.start_motor_oscillation("thumb", 0.5, 3.0)
    ethercat.stop_motor_oscillation("thumb")
    
