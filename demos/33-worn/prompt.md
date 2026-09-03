Make a 9-second piece on a 1440×900 canvas: a radial dark-grey gradient background and the studio scene environment; ONE layer of kind 'stage' named 'Bench', placed 600 px tall in the centre (offset 30 px up), whose keyframes carry the camera in three quick eased-out moves with holds between (yaw -38 to -12 by 1.1 s, hold, to 22 by 4.2 s, hold, to 36 by 7.3 s; pitch 12 down to 5, distance 4.7 in to 3.9) and the key light flying ahead of each move (yaw -130 to -50 to 50 to 125, pitch 65 down to 24, intensity 1.3 to 1.5, drifting during the holds). Its members: a PANEL that is a model resource with no file and a PARTS recipe — a Print box (size [1.6,1.0,0.05], radius 0.015) and a Frame box (size [1.68,1.08,0.04], radius 0.02, positioned at z -0.03) — whose Print slot WEARS the video rec_lumen_2560.mp4 as a surface ("mode": "surface", metallic 0, roughness 0.18) so the light shades it, with the Frame chrome D2D6DC (metallic 1, roughness 0.25), offset 0.42 left and 0.12 up and set back 0.35 in depth; and a VASE that is a model resource with no file and a PARTS recipe of one lathe (slot Body, profile [[0.18,0.75],[0.14,0.62],[0.16,0.5],[0.26,0.35],[0.34,0.15],[0.36,-0.05],[0.33,-0.3],[0.26,-0.55],[0.2,-0.7],[0.22,-0.75],[0,-0.75]], 48 segments) whose Body slot WEARS label_lumen.png as a surface tiled three times round it (repeat [3,1]) over a glaze F2E9DC (metallic 0.05, roughness 0.15), offset 0.66 right and 0.28 down and brought forward 0.15 in depth, turning from a yaw of -90 to 90; and along the bottom a bold caption 'Every picture, lit.' that fades in word by word from 1.2 seconds.

Files in `resources/`: label_lumen.png, rec_lumen_2560.mp4.

Text to use, in order:
- Every picture, lit.

Use the PromoShot skill and the PromoShot MCP tools for this. Work in the
current folder: the media is in `resources/`. Write the project as a
`.promo` folder named `out.promo` here (copy the media you use into its
`Resources/`), validate it, inspect it, render a contact sheet of a few
moments, and render the video. Do not ask questions; make sensible choices.
