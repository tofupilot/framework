import base64
import io
import time
import math
from PIL import Image, ImageDraw

def run(phase, log, measurements, ui):
    for frame_num in range(30):
        img = Image.new('RGB', (640, 480), color=(20, 20, 30))
        draw = ImageDraw.Draw(img)

        t = frame_num / 30.0
        x = int(320 + 200 * math.cos(t * 2 * math.pi))
        y = int(240 + 150 * math.sin(t * 2 * math.pi))

        draw.ellipse([x-20, y-20, x+20, y+20], fill='cyan', outline='white', width=2)

        draw.text((20, 20), f"Live Feed Frame {frame_num + 1}/30", fill='white')
        draw.text((20, 450), "Streaming at ~10 FPS", fill='gray')

        buffer = io.BytesIO()
        img.save(buffer, format='JPEG', quality=85)
        img_base64 = base64.b64encode(buffer.getvalue()).decode('utf-8')

        test.ui.set_value('live_feed', f'data:image/jpeg;base64,{img_base64}')
        test.ui.set_value('frame_count', str(frame_num + 1))

        time.sleep(0.1)

    return test.
