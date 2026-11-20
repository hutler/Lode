"""Lode mascot: a pixel-art anime girl miner (original character), drawn at 64x64.

Run from docs/logo: python source/mascot.py lode-mascot  (needs Pillow)
"""
import sys
from PIL import Image, ImageDraw

N = 64
C = {
    'K': (30, 24, 46, 255),     # outline
    'H': (74, 78, 196, 255),    # hair
    'h': (127, 140, 240, 255),  # hair highlight
    'd': (46, 47, 134, 255),    # hair shadow
    'S': (255, 221, 196, 255),  # skin
    's': (240, 170, 144, 255),  # skin shadow
    'B': (255, 160, 160, 255),  # blush
    'E': (240, 170, 40, 255),   # iris gold
    'e': (150, 80, 20, 255),    # iris dark
    'W': (255, 255, 255, 255),
    'Y': (246, 196, 69, 255),   # helmet
    'y': (201, 141, 30, 255),   # helmet shade
    'L': (255, 248, 200, 255),  # lamp
    'O': (232, 106, 42, 255),   # vest
    'o': (176, 72, 24, 255),    # vest shade
    'T': (236, 236, 244, 255),  # shirt
    't': (190, 190, 210, 255),
    'G': (94, 240, 255, 255),   # gem
    'g': (43, 179, 214, 255),
    'M': (200, 80, 100, 255),   # mouth
    'P': (140, 100, 70, 255),   # pickaxe handle
    'p': (100, 68, 46, 255),
    'I': (170, 180, 196, 255),  # iron
    'i': (110, 118, 136, 255),
}

img = Image.new('RGBA', (N, N), (0, 0, 0, 0))
d = ImageDraw.Draw(img)


def px(x, y, k):
    if 0 <= x < N and 0 <= y < N:
        img.putpixel((x, y), C[k])


def rows(x0, y0, art):
    for j, line in enumerate(art.strip('\n').split('\n')):
        for i, ch in enumerate(line):
            if ch not in ' .':
                px(x0 + i, y0 + j, ch)


# pickaxe behind, over her left shoulder (viewer's right)
d.line([(44, 63), (58, 28)], fill=C['P'], width=3)
d.line([(46, 63), (60, 28)], fill=C['p'], width=1)
d.polygon([(47, 26), (54, 22), (62, 23), (63, 27), (57, 26), (50, 29)], fill=C['I'])
d.line([(50, 29), (57, 26), (63, 27)], fill=C['i'])

# long hair behind
d.polygon([(16, 26), (48, 26), (51, 40), (52, 56), (46, 58), (40, 50),
           (24, 50), (18, 58), (12, 56), (13, 40)], fill=C['H'])
d.line([(14, 42), (15, 54)], fill=C['d'])
d.line([(50, 42), (49, 54)], fill=C['d'])
d.line([(17, 44), (18, 52)], fill=C['h'])

# shoulders: shirt + vest
d.polygon([(12, 63), (15, 55), (24, 51), (40, 51), (49, 55), (52, 63)], fill=C['T'])
d.polygon([(12, 63), (15, 55), (24, 51), (28, 52), (27, 63)], fill=C['O'])
d.polygon([(52, 63), (49, 55), (40, 51), (36, 52), (37, 63)], fill=C['O'])
d.line([(27, 53), (27, 63)], fill=C['o'])
d.line([(37, 53), (37, 63)], fill=C['o'])
d.line([(15, 58), (22, 58)], fill=C['o'])
d.line([(42, 58), (49, 58)], fill=C['o'])
# reflective stripe
d.line([(14, 60), (26, 60)], fill=C['Y'])
d.line([(38, 60), (50, 60)], fill=C['Y'])
# collar
rows(27, 51, """
TT.....TT
tTT...TTt
.tTT.TTt.
""")

# neck
d.rectangle([29, 45, 35, 51], fill=C['S'])
d.line([(29, 47), (35, 47)], fill=C['s'])

# face: rounded top, soft pointed chin
d.polygon([(19, 24), (45, 24), (45, 38), (43, 43), (37, 48), (32, 49),
           (27, 48), (21, 43), (19, 38)], fill=C['S'])
d.line([(21, 43), (27, 48), (31, 49), (33, 49), (37, 48), (43, 43)], fill=C['K'])
d.line([(29, 50), (35, 50)], fill=C['s'])

# side hair framing the face
d.polygon([(16, 24), (21, 26), (20, 44), (18, 51), (15, 46), (14, 32)], fill=C['H'])
d.polygon([(48, 24), (43, 26), (44, 44), (46, 51), (49, 46), (50, 32)], fill=C['H'])
d.line([(17, 30), (17, 44)], fill=C['h'])
d.line([(47, 30), (47, 44)], fill=C['d'])

# helmet dome + brim
d.ellipse([16, 6, 48, 34], fill=C['Y'])
d.rectangle([14, 21, 50, 34], fill=(0, 0, 0, 0))
d.rectangle([13, 21, 51, 24], fill=C['Y'])
d.line([(13, 24), (51, 24)], fill=C['y'])
d.line([(20, 12), (18, 20)], fill=C['W'])
d.line([(41, 10), (46, 19)], fill=C['y'])
d.line([(32, 7), (32, 9)], fill=C['y'])
# lamp
d.ellipse([27, 11, 37, 20], fill=C['K'])
d.ellipse([28, 12, 36, 19], fill=C['L'])
px(30, 14, 'W'); px(31, 14, 'W'); px(30, 15, 'W')

# bangs
rows(18, 25, """
HHHHHHHHHHHHHHHHHHHHHHHHHHHHH
HHhhhHHHHHHHhhhHHHHHHHhhhHHHH
HHHHHHHHHHHHHHHHHHHHHHHHHHHHH
HHHHHHdHHHHHHHHHHHHHdHHHHHHHH
HHHHHd.dHHHHHHHHHHHd.dHHHHHHH
HHHHd...dHHHHHHHHHd...dHHHHHH
dHHd.....dHHHHHHHd.....dHHHHd
.dd.......dHHHHHd.......dHHd.
...........dHHHd.........dd..
............dHd..............
""")

# cheek shading on the light-away side
for y in range(34, 43):
    px(44 if y < 41 else 43, y, 's')

# eyes (left and right), big anime style
eye_l = """
KKKKKKK.
.KeeeeeK
.eWWEEe.
.eWEEEe.
.eEEEEe.
.eEEEEe.
.eEYYEe.
..eeee..
"""
eye_r = "\n".join(line[::-1] for line in eye_l.strip("\n").split("\n"))
rows(21, 32, eye_l)
rows(35, 32, "\n" + eye_r + "\n")
# brows
# blush
for x in (22, 24, 40, 42):
    px(x, 41, 'B')
# nose + smile
px(32, 41, 's')
rows(30, 43, """
M...M
.MMM.
""")

# glowing gem held up (bottom left)
rows(3, 44, """
....K....
...KGK...
..KGWGK..
.KGWGGgK.
KGGGGGggK
.KgGGggK.
..KggggK.
...KggK..
....KK...
""")
# hand holding the gem

# sparkles
for (x, y) in ((2, 43), (13, 47), (11, 42)):
    px(x, y, 'G')
px(1, 43, 'W'); px(3, 43, 'W'); px(2, 42, 'W'); px(2, 44, 'W')

# auto outline: transparent pixels touching opaque get outline colour
src = img.copy()
for y in range(N):
    for x in range(N):
        if src.getpixel((x, y))[3] == 0:
            for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                xx, yy = x + dx, y + dy
                if 0 <= xx < N and 0 <= yy < N and src.getpixel((xx, yy))[3] and src.getpixel((xx, yy)) != C['G'] and src.getpixel((xx, yy)) != C['W']:
                    img.putpixel((x, y), C['K'])
                    break

out = sys.argv[1]
img.save(out + '-64.png')
img.resize((N * 8, N * 8), Image.NEAREST).save(out + '.png')
