#ifndef __SCREENCAPTURE_H__
#define __SCREENCAPTURE_H__

#include <string>

// ---------------------------------------------------------------------------
// CaptureWindowsByPid — capture all visible windows belonging to a process.
//
// Parameters:
//   pid       — target process ID (0 = current process)
//   format    — "jpeg" (default, smaller size, better for AI) or "png" (lossless)
//   quality   — JPEG quality 1-100 (default 80). Ignored for PNG.
//   grayscale — false (default, full color) or true (convert to grayscale;
//               reduces file size, useful when color is not needed by the AI).
//
// Returns a JSON string with base64-encoded image data:
// {
//   "pid": 12345,
//   "windowCount": 2,
//   "format": "jpeg",
//   "quality": 80,
//   "grayscale": false,
//   "windows": [
//     {
//       "hwnd": 65538,            — window handle (decimal)
//       "ownerHwnd": 0,           — owner window handle (0 = top-level / no owner)
//       "ownerIndex": -1,         — index of owner in windows array (-1 = root)
//       "level": 0,               — depth in owner chain (0 = root, 1 = owned by root, …)
//       "zOrder": 0,              — Z-order index in EnumWindows (0 = topmost for this PID)
//       "title": "1С:Предприятие - ...",
//       "className": "V8NewLocalFrameBaseWnd",
//       "x": 100, "y": 100, "width": 1200, "height": 800,
//       "isMainWindow": true,     — true when ownerHwnd == 0
//       "isModal": false,         — true when owner exists and is disabled
//       "isEnabled": true,        — IsWindowEnabled(hwnd)
//       "isMinimized": false,     — IsIconic(hwnd)
//       "isMaximized": false,     — IsZoomed(hwnd)
//       "image": "base64...",
//       "mimeType": "image/jpeg",
//       "imageSize": 123456
//     }
//   ]
// }
//
// On non-Windows platforms returns an error JSON.
// ---------------------------------------------------------------------------

#ifdef _WINDOWS
std::string CaptureWindowsByPid(unsigned long pid,
                                const std::string& format = "jpeg",
                                int quality = 80,
                                bool grayscale = false);
#else
inline std::string CaptureWindowsByPid(unsigned long pid,
                                       const std::string& format = "jpeg",
                                       int quality = 80,
                                       bool grayscale = false) {
    (void)format; (void)quality; (void)grayscale;
    return R"({"error":"Screenshot capture is only supported on Windows","pid":)" + std::to_string(pid) + "}";
}
#endif

#endif // __SCREENCAPTURE_H__
