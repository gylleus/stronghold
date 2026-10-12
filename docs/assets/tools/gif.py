"""gif.py STRIP.png SIZE SCALE OUT.gif DUR,DUR,...  — frames laid side by side → upscaled GIF on a dark ground."""
import sys
from PIL import Image
s = Image.open(sys.argv[1]); z = int(sys.argv[2]); k = int(sys.argv[3]); d = [int(x) for x in sys.argv[5].split(',')]
fr = []
for i in range(s.width // z):
    f = s.crop((i * z, 0, i * z + z, z)); bg = Image.new('RGBA', (z, z), (46, 40, 36, 255)); bg.alpha_composite(f)
    fr.append(bg.resize((z * k, z * k), Image.NEAREST))
fr[0].save(sys.argv[4], save_all=True, append_images=fr[1:], duration=d[:len(fr)], loop=0)
