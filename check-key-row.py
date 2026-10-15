"""Check captured native key rows, not Kotlin source text or JS stubs."""
import json
import re
from pathlib import Path
import xml.etree.ElementTree as ET
from PIL import Image

base = Path(__file__).parent
results = []
for name, fill, tint in [
    ('light-keyboard', (250, 248, 244), (139, 94, 48)),
    ('dark-keyboard', (48, 44, 38), (219, 179, 120)),
]:
    root = ET.parse(base / f'{name}.xml')
    nodes = {n.get('content-desc'): n for n in root.iter('node') if n.get('content-desc') in ('Attach image', 'Esc')}
    bounds = {key: tuple(map(int, re.findall(r'\d+', node.get('bounds')))) for key, node in nodes.items()}
    image = Image.open(base / f'{name}.png').convert('RGB')
    a = bounds['Attach image']
    e = bounds['Esc']
    width, height = a[2]-a[0], a[3]-a[1]
    assert (width, height) == (e[2]-e[0], e[3]-e[1]), (name, bounds)
    assert nodes['Attach image'].get('clickable') == 'true'
    assert nodes['Attach image'].get('enabled') == 'true'
    # Sample above the glyph, away from the rounded corners.
    samples = [image.getpixel(((b[0]+b[2])//2, b[1]+8)) for b in (a, e)]
    assert samples == [fill, fill], (name, samples)
    pixels = [(x, y) for y in range(a[1], a[3]) for x in range(a[0], a[2])
              if max(abs(c-t) for c, t in zip(image.getpixel((x, y)), tint)) < 25]
    assert pixels, name
    ink = (min(x for x, y in pixels), min(y for x, y in pixels), max(x for x, y in pixels)+1, max(y for x, y in pixels)+1)
    # A 24dp drawable fits inside the unchanged 44dp hit target.
    max_glyph = round(width * 24 / 44) + 1
    assert 0 < ink[2]-ink[0] <= max_glyph, (name, ink, max_glyph)
    assert 0 < ink[3]-ink[1] <= max_glyph, (name, ink, max_glyph)
    assert abs((ink[0]+ink[2]) / 2 - (a[0]+a[2]) / 2) <= 2, (name, ink)
    assert abs((ink[1]+ink[3]) / 2 - (a[1]+a[3]) / 2) <= 2, (name, ink)
    results.append({'capture': name, 'hit_target_px': [width, height], 'ink_px': [ink[2]-ink[0], ink[3]-ink[1]], 'background_rgb': samples[0], 'passed': True})
print(json.dumps(results, indent=2))
