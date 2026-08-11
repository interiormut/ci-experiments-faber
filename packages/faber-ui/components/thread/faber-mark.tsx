"use client"

import * as React from "react"
import { motion, useReducedMotion } from "framer-motion"

import { FaberLogo } from "@/components/ui/logos"
import { PANIT_DEFAULT_EASE } from "@/lib/motion"
import { cn } from "@/lib/utils"

/**
 * The Faber mark, idle or in flight.
 *
 * Two artworks, not one: the plain logo (the head) and the winged logo are
 * separate marks — the winged one is not a superset of the plain one, so
 * neither a clip reveal nor a viewBox zoom can turn one into the other. So the
 * container is fixed and both layers are simply centred in it, crossfading in
 * place. No per-artwork alignment.
 */

// Tight bounding box of the winged artwork inside its 331.79² document.
const ART_X = 46.12
const ART_Y = 72.66
const ART_W = 243.53
const ART_H = 172.86

/** Box aspect. The mark is sized by height; width follows. */
export const MARK_ASPECT = ART_W / ART_H

export function FaberMark({
  className,
  size = 18,
  working = false,
}: {
  className?: string
  /** Height of the mark in pixels; the box is `size * MARK_ASPECT` wide. */
  size?: number
  working?: boolean
}) {
  const reduce = useReducedMotion()
  const transition = reduce
    ? { duration: 0 }
    : { duration: 0.45, ease: PANIT_DEFAULT_EASE }

  return (
    <span
      aria-hidden
      className={cn("relative block shrink-0", className)}
      style={{ width: size * MARK_ASPECT, height: size }}
    >
      <motion.span
        className="absolute inset-0 flex items-center justify-center"
        initial={false}
        animate={{ opacity: working ? 0 : 1 }}
        transition={transition}
      >
        {/* Sized by the box's *width*: the plain logo is a square image whose
            ink spans its full width but is letterboxed vertically, so matching
            heights would draw it noticeably smaller than the winged mark. */}
        <FaberLogo size={size * MARK_ASPECT} aria-hidden />
      </motion.span>

      <motion.svg
        className="absolute inset-0"
        viewBox={`${ART_X} ${ART_Y} ${ART_W} ${ART_H}`}
        width={size * MARK_ASPECT}
        height={size}
        initial={false}
        animate={{ opacity: working ? 1 : 0, scale: working || reduce ? 1 : 0.9 }}
        transition={transition}
      >
        {/* Navy first, blue over it — the overlap is what draws the white
            separation between wing and breast. Do not reorder. */}
        <path
          fill="#151e3a"
          d="m 124.35417,245.53333 c 29.01371,0 56.76344,-5.42244 81.49166,-21.34979 16.15814,-10.40741 29.24189,-25.0319 45.50834,-35.15855 6.61541,-4.11841 13.95515,-6.98683 21.43125,-9.10832 -3.11783,-1.65754 -8.17103,-1.29655 -11.64167,-1.63241 -6.31384,-0.611 -14.12867,-1.78956 -20.37292,-0.3879 -3.84299,0.86264 -7.55687,3.21641 -11.1125,4.84906 -15.20341,6.98098 -30.18428,14.42246 -45.24375,21.70911 -10.39019,5.02739 -20.78314,10.05127 -30.69166,15.99626 -8.03976,4.82377 -15.84004,10.23134 -22.48959,16.88046 -2.38508,2.38493 -5.63955,4.97941 -6.87916,8.20208 z"
        />
        <path
          fill="#133db5"
          d="m 58.208331,72.760415 c 0.0037,5.596929 1.680352,11.395106 3.439584,16.66875 7.469578,22.391565 24.360932,37.245995 44.450005,48.381995 6.59184,3.65407 14.60314,8.3434 21.96041,10.09092 v 0.26459 c -28.572785,-5.91334 -54.792261,-19.96602 -81.756249,-30.42709 0.80518,5.97443 4.504635,12.121 7.567368,17.19792 6.095607,10.10434 14.348252,18.99829 24.447216,25.18543 9.76817,5.98447 20.599018,9.31036 31.485415,12.57588 6.83386,2.04991 14.80994,5.32258 21.96042,5.63036 v 0.26458 l -29.89792,-2.60112 -35.718749,-4.27805 c 2.346684,6.1894 9.866073,11.81521 15.081251,15.53345 15.498989,11.05027 34.743878,16.72499 53.710418,17.53947 -8.482,8.48201 -17.95624,15.82917 -27.25208,23.38624 -2.94612,2.39504 -5.85669,4.82475 -8.731255,7.30543 -1.282221,1.10652 -2.93078,2.17659 -3.704166,3.70416 33.285731,-15.83088 66.828221,-31.10763 100.012501,-47.16422 11.15271,-5.39636 22.34253,-10.71894 33.60208,-15.88827 6.11434,-2.80713 12.40831,-6.60689 19.05,-7.94443 4.65889,-0.93824 10.10913,-0.12797 14.81667,0.0782 8.87498,0.38861 17.845,1.06875 26.72292,1.06875 -2.39062,-3.39367 -6.11807,-5.69329 -9.525,-7.9383 -7.4635,-4.91811 -15.6692,-8.44618 -23.28232,-13.05688 -1.58522,-0.96005 -2.5621,-2.73966 -3.72663,-4.14023 -1.91411,-2.30209 -4.03738,-4.56512 -6.32855,-6.49843 -10.04924,-8.47965 -23.0031,-12.23698 -35.98334,-10.05518 -8.24421,1.38573 -15.63858,5.55491 -21.96041,10.88895 -2.02637,1.70975 -3.94321,3.53018 -5.82084,5.40007 -0.59134,0.5889 -1.42168,1.74269 -2.38125,1.55648 -2.24606,-0.43587 -5.42631,-4.24506 -7.14375,-5.74857 -4.2316,-3.70452 -8.9275,-7.27082 -14.02291,-9.70221 C 136.36622,119.10558 113.07163,108.21119 91.281249,95.144256 83.88489,90.708916 76.603632,86.132855 69.585415,81.115606 65.95171,78.517912 62.242044,74.608154 58.208331,72.760415 M 223.57292,144.81406 c 7.18263,-1.26129 9.45343,9.93736 2.38125,11.20293 -7.50137,1.34238 -9.91729,-9.87958 -2.38125,-11.20293"
        />
      </motion.svg>
    </span>
  )
}
