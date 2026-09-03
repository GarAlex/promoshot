Make a 10-second piece on a 1440×900 canvas: a linear gradient background from 1B2440 through 0B0D12 to 2A1638 and the studio scene environment; ONE layer of kind 'stage' named 'Bench', placed 640 px tall in the centre (offset 30 px up), whose keyframes carry a key light that stays at yaw 35, pitch 32, intensity 1.4 while the camera holds a yaw of -25 and only eases in (pitch 18 to 14, distance 4.7 to 4.3 by 4.9 s), then settles to yaw -10, pitch 6, distance 4.0 by 8.6 s as the light moves to yaw 60, pitch 28. Its members: a CUBE that is a model resource with no file and a PARTS recipe of one box (size [1.4,1.4,1.4], radius 0.05, "faces": true) whose six slots Cube/front, Cube/right, Cube/back, Cube/left, Cube/top, Cube/bottom each WEAR one of face_1.png … face_6.png as a surface (roughness 0.35), living the whole piece and spinning once and a bit, from a yaw of 0 to 380 linearly over 5.2 s, so each lit side passes the light; a WORD that is a model resource with a TEXT recipe 'PROMO' (bold, depth 0.35, size 0.5) with a chrome Face D8DDE6 (metallic 1, roughness 0.18) and a Side 5B8CFF (metallic 0.4, roughness 0.4), living the whole piece; and POINTS: a particles resource with a MORPH from the cube to the word (count 3000, spread 1.1, size 0.013, turbulence 0.2, stagger 0.45, colors @accent, FFFFFF, FFB050, seed 11) played by a DRAWING member living the whole piece whose keyframes hold progress 0 until 4.9 s, burst to 0.45 by 5.6 s (ease out), drift to 0.6 by 7.0 s, and gather to 1 by 8.4 s (ease in) — the morph dissolves the cube as the points leave and assembles the word as they land; and along the bottom from 8.5 s a bold caption 'One format. Real 3D.' that fades in word by word.

Files in `resources/`: face_1.png, face_2.png, face_3.png, face_4.png, face_5.png, face_6.png.

Text to use, in order:
- One format. Real 3D.

Use the PromoShot skill and the PromoShot MCP tools for this. Work in the
current folder: the media is in `resources/`. Write the project as a
`.promo` folder named `out.promo` here (copy the media you use into its
`Resources/`), validate it, inspect it, render a contact sheet of a few
moments, and render the video. Do not ask questions; make sensible choices.
