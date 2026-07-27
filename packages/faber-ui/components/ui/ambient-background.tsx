"use client"

import type { CSSProperties } from "react"
import { cn } from "@/lib/utils"

type Blob = {
  color: string
  size: string
  top: string
  left: string
  animation: string
  duration: string
  delay: string
}

const MESH_BASE = "#F7FAFF"

const BLOBS: Blob[] = [
  {
    color: "#F8FBFF",
    size: "68vw",
    top: "-8%",
    left: "-6%",
    animation: "mesh-drift-a",
    duration: "22s",
    delay: "0s",
  },
  {
    color: "#F3F8FF",
    size: "60vw",
    top: "20%",
    left: "55%",
    animation: "mesh-drift-b",
    duration: "28s",
    delay: "-6s",
  },
  {
    color: "#EDF4FF",
    size: "56vw",
    top: "50%",
    left: "8%",
    animation: "mesh-drift-c",
    duration: "25s",
    delay: "-12s",
  },
  {
    color: "#E4EFFF",
    size: "50vw",
    top: "55%",
    left: "60%",
    animation: "mesh-drift-d",
    duration: "31s",
    delay: "-3s",
  },
  {
    color: "#DBEAFF",
    size: "48vw",
    top: "10%",
    left: "28%",
    animation: "mesh-drift-c",
    duration: "34s",
    delay: "-18s",
  },
]

export type AmbientBackgroundProps = {
  debug?: boolean
  className?: string
}

export function AmbientBackground({ debug = false, className }: AmbientBackgroundProps) {
  return (
    <>
      <style>{`
        @keyframes mesh-drift-a {
          0% { transform: translate(0, 0) scale(1); }
          25% { transform: translate(14%, -10%) scale(1.15); }
          50% { transform: translate(-8%, 16%) scale(0.85); }
          75% { transform: translate(10%, 6%) scale(1.08); }
          100% { transform: translate(0, 0) scale(1); }
        }
        @keyframes mesh-drift-b {
          0% { transform: translate(0, 0) scale(1); }
          25% { transform: translate(-16%, 8%) scale(0.9); }
          50% { transform: translate(10%, -14%) scale(1.22); }
          75% { transform: translate(-8%, -4%) scale(1.03); }
          100% { transform: translate(0, 0) scale(1); }
        }
        @keyframes mesh-drift-c {
          0% { transform: translate(0, 0) scale(1); }
          25% { transform: translate(8%, 14%) scale(1.1); }
          50% { transform: translate(-14%, -8%) scale(0.88); }
          75% { transform: translate(4%, -10%) scale(1.12); }
          100% { transform: translate(0, 0) scale(1); }
        }
        @keyframes mesh-drift-d {
          0% { transform: translate(0, 0) scale(1) rotate(0deg); }
          25% { transform: translate(-10%, -8%) scale(1.18) rotate(6deg); }
          50% { transform: translate(8%, 10%) scale(0.82) rotate(-5deg); }
          75% { transform: translate(-4%, 4%) scale(1.1) rotate(4deg); }
          100% { transform: translate(0, 0) scale(1) rotate(0deg); }
        }
      `}</style>
      <div
        aria-hidden={!debug}
        className={cn("pointer-events-none absolute inset-0 -z-10 overflow-hidden", className)}
        style={{ backgroundColor: MESH_BASE }}
      >
        {BLOBS.map((blob, i) => {
          const style: CSSProperties = {
            width: blob.size,
            height: blob.size,
            top: blob.top,
            left: blob.left,
            background: `radial-gradient(circle closest-side at 50% 50%, ${blob.color} 0%, ${blob.color}b3 25%, ${blob.color}66 50%, ${blob.color}1a 75%, ${blob.color}00 100%)`,
            animationName: blob.animation,
            animationDuration: blob.duration,
            animationDelay: blob.delay,
            animationTimingFunction: "cubic-bezier(0.45, 0, 0.55, 1)",
            animationIterationCount: "infinite",
            animationDirection: "alternate",
            ...(debug && {
              border: "2px solid red",
              position: "absolute",
              outline: "1px dashed rgba(255,0,0,0.3)",
            }),
          }
          return (
            <div key={i} className="absolute rounded-full" style={style}>
              {debug && (
                <span
                  className="absolute bottom-0 left-0 whitespace-nowrap rounded bg-red-500 px-1 text-[10px] leading-none text-white"
                  style={{ zIndex: 9999 }}
                >
                  blob #{i} ({blob.size} / {blob.top} / {blob.left})
                </span>
              )}
            </div>
          )
        })}
      </div>
    </>
  )
}
