#!/bin/zsh
# The demo's song, made here so it carries no licence but ours (CC0):
# 128 BPM in 4/4, beat 1 at 0.25 s, fifteen bars, then half a second of tail.
#
#   bars 1-2   intro: kick on every beat, eighth hats, the bass quieter
#   bars 3-8   groove: + clap on 2 and 4, a sine bass struck with each kick and gone before the next (A F C G, a bar each)
#              — a bass that swells after the kick, or sounds across it, pulls the measured beat late
#   bars 9-14  lift: crash on bar 9, louder hats and a ride on the beats, the bass up, a minor pad
#   bar 15     the last hit: crash, bass and chord dying away over soft kicks and ticks
#
# Every sound is an aevalsrc expression of t (noise from ffmpeg's seeded
# random()), so the same ffmpeg writes the same file.
#
#   demos/36-beat-cut/make-music.sh [out.wav]     (default demos/_media/lift_song.wav)
set -e
HERE="${0:A:h}"
OUT="${1:-$HERE/../_media/lift_song.wav}"
FFMPEG="${FFMPEG:-/opt/homebrew/bin/ffmpeg}"
LEN=28.875   # 0.25 + 15 bars x 1.875 s + 0.5 s tail
SR=44100

# Shared clock: 0 = t since beat 1, 1 = beat index n, 2 = phase in the beat,
# 3 = bar index k (0-based), 4 = beat in bar m (0-based).
CLK="st(0,t-0.25);st(1,floor(ld(0)/0.46875));st(2,ld(0)-ld(1)*0.46875);st(3,floor(ld(1)/4));st(4,mod(ld(1),4))"
ON="gte(ld(0),0)"
INTRO="lt(ld(3),2)"; GROOVE="gte(ld(3),2)*lt(ld(3),8)"; LIFT="gte(ld(3),8)*lt(ld(3),14)"; FIN="eq(ld(3),14)"
ROOT="(55*eq(mod(ld(3),4),0)+43.654*eq(mod(ld(3),4),1)+65.406*eq(mod(ld(3),4),2)+48.999*eq(mod(ld(3),4),3))"

KICK="$CLK;$ON*(lt(ld(3),14)+$FIN*(0.6+0.4*eq(ld(4),0)))*(0.8+0.15*eq(ld(4),0))*sin(2*PI*(45*ld(2)+4*(1-exp(-25*ld(2)))))*exp(-6*ld(2))"
CLAP="$CLK;$ON*($GROOVE+$LIFT)*(eq(ld(4),1)+eq(ld(4),3))*(random(9)*2-1)*(exp(-ld(2)*22)+0.4*exp(-max(ld(2)-0.012,0)*30)*gte(ld(2),0.012))"
# eighths accented on the beat (a softer offbeat keeps the pulse on the beat), a ride on the
# lift's beats, quarter ticks through the last bar
HATS="$CLK;st(5,mod(ld(0),0.234375));st(6,lt(ld(2),0.234375));$ON*(($INTRO+$GROOVE)*(0.12+0.38*ld(6))*exp(-ld(5)*90)+$LIFT*((0.16+0.44*ld(6))*exp(-ld(5)*70)+0.3*ld(6)*exp(-ld(2)*9))+$FIN*0.5*exp(-ld(2)*40))*(random(8)*2-1)"
BASS="$CLK;st(6,$ROOT);$ON*((0.7*$INTRO+$GROOVE+1.2*$LIFT)*exp(-ld(2)*3)+$FIN*1.2*exp(-(ld(0)-26.25)*0.8))*(1-exp(-ld(2)*400))*(1-pow(ld(2)/0.46875,6))*(sin(2*PI*ld(6)*t)+0.45*sin(4*PI*ld(6)*t))"
# A minor-ish triad on the bar's root, two octaves up, gated to the beat
# like the bass (a held chord's beating moves the measured beat); dying away through the last bar
PAD="$CLK;st(6,$ROOT*4);($LIFT*exp(-ld(2)*1.5)+$FIN*exp(-(ld(0)-26.25)*0.8))*(1-exp(-ld(2)*400))*(1-pow(ld(2)/0.46875,6))*((2*(ld(6)*t-floor(ld(6)*t))-1)+(2*(ld(6)*1.1892*t-floor(ld(6)*1.1892*t))-1)+(2*(ld(6)*1.4983*t-floor(ld(6)*1.4983*t))-1))/3"
# crash on bar 9 and bar 15
CRASH="$CLK;(gte(ld(0),15)*exp(-(ld(0)-15)*1.8)+gte(ld(0),26.25)*exp(-(ld(0)-26.25)*1.4))*(random(7)*2-1)"

"$FFMPEG" -v error -y \
  -f lavfi -i "aevalsrc='$KICK':s=$SR:d=$LEN" \
  -f lavfi -i "aevalsrc='$CLAP':s=$SR:d=$LEN" \
  -f lavfi -i "aevalsrc='$HATS':s=$SR:d=$LEN" \
  -f lavfi -i "aevalsrc='$BASS':s=$SR:d=$LEN" \
  -f lavfi -i "aevalsrc='$PAD':s=$SR:d=$LEN" \
  -f lavfi -i "aevalsrc='$CRASH':s=$SR:d=$LEN" \
  -filter_complex "\
[0]volume=0.95[k];\
[1]bandpass=f=1400:w=1.2,volume=1.6[c];\
[2]highpass=f=7000,highpass=f=7000,volume=0.55[h];\
[3]volume=0.55[b];\
[4]lowpass=f=2600,volume=0.6[p];\
[5]highpass=f=4500,volume=0.35[x];\
[k][c][h][b][p][x]amix=inputs=6:normalize=0,alimiter=limit=0.89:level=0,afade=t=out:st=28.375:d=0.5[out]" \
  -map "[out]" -ac 1 -ar $SR -c:a pcm_s16le -bitexact "$OUT"
echo "$OUT"
