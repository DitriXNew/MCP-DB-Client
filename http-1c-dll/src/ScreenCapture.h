#ifndef __SCREENCAPTURE_H__
#define __SCREENCAPTURE_H__

#include <string>

// ---------------------------------------------------------------------------
// CaptureWindowsByPid — capture all visible windows belonging to a process.
//
// Parameters:
//   pid      — target process ID (0 = current process)
//   format   — "jpeg" (default, smaller size, better for AI) or "png" (lossless)
//   quality  — JPEG quality 1-100 (default 80). Ignored for PNG.
//
// Returns a JSON string with base64-encoded image data:
// {
//   "pid": 12345,
//   "windowCount": 2,
//   "format": "jpeg",
//   "quality": 80,
//   "windows": [
//     {
//       "hwnd": 65538,
//       "title": "1С:Предприятие - ...",
//       "className": "V8NewLocalFrameBaseWnd",
//       "x": 100, "y": 100, "width": 1200, "height": 800,
//       "isMainWindow": true,
//       "isModal": false,
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
                                int quality = 80);
#else
inline std::string CaptureWindowsByPid(unsigned long pid,
                                       const std::string& format = "jpeg",
                                       int quality = 80) {
    (void)format; (void)quality;
    return R"({"error":"Screenshot capture is only supported on Windows","pid":)" + std::to_string(pid) + "}";
}
#endif

#endif // __SCREENCAPTURE_H__
