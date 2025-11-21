import base64
import io
import random
from PIL import Image, ImageDraw

def run(test, ui):
    img = Image.new('RGB', (640, 480), color=(30, 30, 40))
    draw = ImageDraw.Draw(img)

    for _ in range(100):
        x = random.randint(0, 640)
        y = random.randint(0, 480)
        r = random.randint(1, 5)
        color = (
            random.randint(150, 255),
            random.randint(150, 255),
            random.randint(150, 255)
        )
        draw.ellipse([x-r, y-r, x+r, y+r], fill=color)

    draw.text((20, 20), "Simulated Camera Capture", fill='white')
    draw.text((20, 450), f"Timestamp: {test.run.started_at}", fill='white')

    buffer = io.BytesIO()
    img.save(buffer, format='PNG')
    img_base64 = base64.b64encode(buffer.getvalue()).decode('utf-8')

    test.ui.set_value('captured_image', f'data:image/png;base64,{img_base64}')

    return test.ui.depends_on_input([])
