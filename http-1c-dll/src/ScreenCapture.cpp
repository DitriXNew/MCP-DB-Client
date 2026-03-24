#ifdef _WINDOWS

#include "ScreenCapture.h"

#include <windows.h>
#ifndef WS_EX_NOREDIRECTIONBITMAP
#define WS_EX_NOREDIRECTIONBITMAP 0x00200000L
#endif
#include <dwmapi.h>    // DwmGetWindowAttribute (DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_CLOAKED)
#include <objidl.h>    // IStream — must come before gdiplus.h when WIN32_LEAN_AND_MEAN is defined
#include <gdiplus.h>
#pragma comment(lib, "gdiplus.lib")
#pragma comment(lib, "ole32.lib")
#include <vector>
#include <string>
#include <mutex>

#include "json.hpp"

using json = nlohmann::json;

namespace {

// =========================================================================
// Base64 encoder
// =========================================================================

static const char kBase64Table[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

std::string Base64Encode(const BYTE* data, size_t len) {
    std::string out;
    out.reserve(((len + 2) / 3) * 4);
    for (size_t i = 0; i < len; i += 3) {
        unsigned int n = static_cast<unsigned int>(data[i]) << 16;
        if (i + 1 < len) n |= static_cast<unsigned int>(data[i + 1]) << 8;
        if (i + 2 < len) n |= static_cast<unsigned int>(data[i + 2]);
        out.push_back(kBase64Table[(n >> 18) & 0x3F]);
        out.push_back(kBase64Table[(n >> 12) & 0x3F]);
        out.push_back((i + 1 < len) ? kBase64Table[(n >>  6) & 0x3F] : '=');
        out.push_back((i + 2 < len) ? kBase64Table[ n        & 0x3F] : '=');
    }
    return out;
}

// =========================================================================
// GDI+ lazy initialization (once per process lifetime)
// =========================================================================

static ULONG_PTR g_gdiplusToken = 0;
static std::once_flag g_gdiplusOnce;

void EnsureGdiplus() {
    std::call_once(g_gdiplusOnce, []() {
        Gdiplus::GdiplusStartupInput input;
        Gdiplus::GdiplusStartup(&g_gdiplusToken, &input, nullptr);
    });
}

// =========================================================================
// Get CLSID for an image encoder by MIME type
// =========================================================================

bool GetEncoderClsid(const wchar_t* mimeType, CLSID* clsid) {
    UINT num = 0, size = 0;
    Gdiplus::GetImageEncodersSize(&num, &size);
    if (size == 0) return false;

    std::vector<BYTE> buf(size);
    auto* encoders = reinterpret_cast<Gdiplus::ImageCodecInfo*>(buf.data());
    Gdiplus::GetImageEncoders(num, size, encoders);

    for (UINT i = 0; i < num; ++i) {
        if (wcscmp(encoders[i].MimeType, mimeType) == 0) {
            *clsid = encoders[i].Clsid;
            return true;
        }
    }
    return false;
}

// =========================================================================
// Capture a single HWND into an image byte buffer (JPEG or PNG).
//
// Uses PrintWindow with PW_RENDERFULLCONTENT (0x02) for DWM-aware capture
// that works even when the window is partially occluded or off-screen.
// Falls back to the basic PrintWindow flag on older systems.
// =========================================================================

std::vector<BYTE> CaptureWindowToImage(HWND hwnd, const CLSID& encoderClsid,
                                       Gdiplus::EncoderParameters* encoderParams,
                                       bool grayscale) {
    std::vector<BYTE> result;

    RECT rc;
    if (!GetWindowRect(hwnd, &rc)) return result;

    int w = rc.right - rc.left;
    int h = rc.bottom - rc.top;
    if (w <= 0 || h <= 0) return result;

    // Get the actual visible bounds (without invisible DWM shadow/frame).
    // On Windows 10/11, GetWindowRect includes ~7px invisible border on each side.
    RECT visibleRect = rc;
    DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS,
                          &visibleRect, sizeof(visibleRect));

    // Calculate crop offsets relative to the full window rect.
    int cropLeft   = visibleRect.left   - rc.left;
    int cropTop    = visibleRect.top    - rc.top;
    int cropWidth  = visibleRect.right  - visibleRect.left;
    int cropHeight = visibleRect.bottom - visibleRect.top;

    // Clamp to valid bounds.
    if (cropLeft < 0) cropLeft = 0;
    if (cropTop  < 0) cropTop  = 0;
    if (cropWidth  <= 0 || cropWidth  > w) cropWidth  = w;
    if (cropHeight <= 0 || cropHeight > h) cropHeight = h;
    if (cropLeft + cropWidth  > w) cropWidth  = w - cropLeft;
    if (cropTop  + cropHeight > h) cropHeight = h - cropTop;

    HDC hdcScreen = GetDC(nullptr);
    HDC hdcMem    = CreateCompatibleDC(hdcScreen);
    HBITMAP hBmp  = CreateCompatibleBitmap(hdcScreen, w, h);
    HGDIOBJ oldBmp = SelectObject(hdcMem, hBmp);

    // Attempt 1: PrintWindow with PW_RENDERFULLCONTENT (0x02).
    // Works for regular GDI windows and, on Windows 11 / 10 21H2+, also for
    // DWM-composed GDI windows regardless of occlusion.
    BOOL captured = PrintWindow(hwnd, hdcMem, 0x00000002);

    if (!captured) {
        // Attempt 2: basic PrintWindow (older GDI / Windows 7 compatibility).
        captured = PrintWindow(hwnd, hdcMem, 0);
    }

    if (!captured) {
        // Attempt 3: for DirectComposition windows (1C:Enterprise 8.5+ new UI,
        // WS_EX_NOREDIRECTIONBITMAP) PrintWindow cannot render into a GDI DC at all.
        // Workaround: temporarily make the window topmost WITHOUT stealing keyboard
        // focus (SWP_NOACTIVATE), wait for DWM to compose exactly one frame
        // (DwmFlush), then BitBlt from the screen DC.  After capture, restore the
        // original z-order flag.  The window pops to the front for one frame only —
        // imperceptible to the user in normal usage.
        bool madeTopmost = false;
        LONG_PTR exNow = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if (!(exNow & WS_EX_TOPMOST)) {
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                         SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            madeTopmost = true;
        }
        // Wait for the compositor to present the new z-order.
        DwmFlush();

        captured = BitBlt(hdcMem, 0, 0, w, h, hdcScreen, rc.left, rc.top, SRCCOPY);

        if (madeTopmost) {
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0,
                         SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    SelectObject(hdcMem, oldBmp);

    if (captured) {
        Gdiplus::Bitmap fullBmp(hBmp, nullptr);

        // Crop to visible area (removes invisible DWM borders).
        Gdiplus::Bitmap* croppedBmp = fullBmp.Clone(
            cropLeft, cropTop, cropWidth, cropHeight,
            PixelFormatDontCare);

        Gdiplus::Bitmap* bmpToSave = croppedBmp ? croppedBmp : &fullBmp;

        // Optional grayscale conversion using a GDI+ ColorMatrix.
        // Luminance formula: L = 0.299*R + 0.587*G + 0.114*B.
        Gdiplus::Bitmap* gsBmp = nullptr;
        if (grayscale) {
            Gdiplus::ColorMatrix gsMatrix = {{
                { 0.299f, 0.299f, 0.299f, 0, 0 },
                { 0.587f, 0.587f, 0.587f, 0, 0 },
                { 0.114f, 0.114f, 0.114f, 0, 0 },
                { 0,      0,      0,      1, 0 },
                { 0,      0,      0,      0, 1 }
            }};
            Gdiplus::ImageAttributes attrs;
            attrs.SetColorMatrix(&gsMatrix,
                Gdiplus::ColorMatrixFlagsDefault,
                Gdiplus::ColorAdjustTypeBitmap);
            int gw = static_cast<int>(bmpToSave->GetWidth());
            int gh = static_cast<int>(bmpToSave->GetHeight());
            gsBmp = new Gdiplus::Bitmap(gw, gh, PixelFormat24bppRGB);
            Gdiplus::Graphics* g = Gdiplus::Graphics::FromImage(gsBmp);
            if (g) {
                Gdiplus::Rect dest(0, 0, gw, gh);
                g->DrawImage(bmpToSave, dest, 0, 0, gw, gh,
                             Gdiplus::UnitPixel, &attrs);
                delete g;
                bmpToSave = gsBmp;  // only switch if draw succeeded
            } else {
                // Graphics creation failed; discard gsBmp and keep color bitmap.
                delete gsBmp;
                gsBmp = nullptr;
            }
        }

        IStream* stream = nullptr;
        if (CreateStreamOnHGlobal(nullptr, TRUE, &stream) == S_OK) {
            if (bmpToSave->Save(stream, &encoderClsid, encoderParams) == Gdiplus::Ok) {
                STATSTG stat = {};
                stream->Stat(&stat, STATFLAG_NONAME);
                ULONG totalSize = stat.cbSize.LowPart;
                result.resize(totalSize);
                LARGE_INTEGER li = {};
                stream->Seek(li, STREAM_SEEK_SET, nullptr);
                ULONG bytesRead = 0;
                stream->Read(result.data(), totalSize, &bytesRead);
                result.resize(bytesRead);
            }
            stream->Release();
        }

        delete gsBmp;
        delete croppedBmp;
    }

    DeleteObject(hBmp);
    DeleteDC(hdcMem);
    ReleaseDC(nullptr, hdcScreen);

    return result;
}

// =========================================================================
// Window information collected during enumeration
// =========================================================================

struct WindowInfo {
    HWND   hwnd;
    HWND   ownerHwnd;
    std::wstring title;
    std::wstring className;
    RECT   rect;
    bool   isMainWindow;
    bool   isModal;
    bool   isEnabled;    // IsWindowEnabled(hwnd)
    bool   isMinimized;  // IsIconic(hwnd)
    bool   isMaximized;  // IsZoomed(hwnd)
    int    zOrder;       // index in EnumWindows z-order (0 = topmost for this PID)
};

struct EnumContext {
    DWORD targetPid;
    std::vector<WindowInfo> windows;
};

BOOL CALLBACK EnumWindowsCallback(HWND hwnd, LPARAM lParam) {
    auto* ctx = reinterpret_cast<EnumContext*>(lParam);

    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);
    if (pid != ctx->targetPid) return TRUE;

    if (!IsWindowVisible(hwnd)) return TRUE;

    // Filter DWM shadow/ghost windows (cloaked by the compositor).
    DWORD cloaked = 0;
    DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &cloaked, sizeof(cloaked));
    if (cloaked != 0) return TRUE;

    // Filter transparent overlay windows (DWM shadows are WS_EX_LAYERED + WS_EX_TRANSPARENT).
    LONG_PTR exStyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    if ((exStyle & WS_EX_LAYERED) && (exStyle & WS_EX_TRANSPARENT)) return TRUE;

    // NOTE: WS_EX_NOREDIRECTIONBITMAP is set by 1C:Enterprise 8.5+ (DirectComposition
    // rendering) and by UWP frame hosts. We no longer skip these — they are real
    // application windows. Capture falls back to BitBlt from screen for them.

    RECT rc;
    if (!GetWindowRect(hwnd, &rc)) return TRUE;
    if (rc.right - rc.left <= 0 || rc.bottom - rc.top <= 0) return TRUE;

    WindowInfo info;
    info.hwnd = hwnd;
    // Use DWM extended frame bounds for accurate visible area (no invisible borders).
    RECT visibleRc = rc;
    DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS,
                          &visibleRc, sizeof(visibleRc));

    // Skip tiny windows (< 30px) — likely DWM shadow remnants.
    if (visibleRc.right - visibleRc.left < 30 || visibleRc.bottom - visibleRc.top < 30)
        return TRUE;

    info.rect = visibleRc;

    wchar_t titleBuf[512] = {};
    GetWindowTextW(hwnd, titleBuf, _countof(titleBuf));
    info.title = titleBuf;

    wchar_t classBuf[256] = {};
    GetClassNameW(hwnd, classBuf, _countof(classBuf));
    info.className = classBuf;

    // Modal window detection:
    // - Get the owner window (GW_OWNER).
    // - If the owner exists and is *disabled*, the current window is modal
    //   because it blocks interaction with the owner.
    info.ownerHwnd   = GetWindow(hwnd, GW_OWNER);
    info.isMainWindow = (info.ownerHwnd == nullptr);
    info.isModal      = false;
    if (info.ownerHwnd != nullptr) {
        info.isModal = !IsWindowEnabled(info.ownerHwnd);
    }
    info.isEnabled   = (IsWindowEnabled(hwnd) != FALSE);
    info.isMinimized = (IsIconic(hwnd)         != FALSE);
    info.isMaximized = (IsZoomed(hwnd)         != FALSE);
    info.zOrder      = static_cast<int>(ctx->windows.size());

    ctx->windows.push_back(info);
    return TRUE;
}

// =========================================================================
// Wide string → UTF-8 helper
// =========================================================================

std::string WideToUtf8(const std::wstring& wstr) {
    if (wstr.empty()) return {};
    int size = WideCharToMultiByte(CP_UTF8, 0, wstr.data(),
        static_cast<int>(wstr.size()), nullptr, 0, nullptr, nullptr);
    std::string out(size, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wstr.data(),
        static_cast<int>(wstr.size()), out.data(), size, nullptr, nullptr);
    return out;
}

} // anonymous namespace

// =========================================================================
// Public API — CaptureWindowsByPid
// =========================================================================

std::string CaptureWindowsByPid(unsigned long pid,
                                const std::string& format, int quality,
                                bool grayscale) {
    if (pid == 0) {
        pid = GetCurrentProcessId();
    }

    // Clamp quality to valid range.
    if (quality < 1) quality = 1;
    if (quality > 100) quality = 100;

    // Determine format: jpeg (default) or png.
    bool useJpeg = (format != "png");
    const wchar_t* mimeTypeW = useJpeg ? L"image/jpeg" : L"image/png";
    std::string mimeTypeStr = useJpeg ? "image/jpeg" : "image/png";

    EnsureGdiplus();

    CLSID encoderClsid;
    if (!GetEncoderClsid(mimeTypeW, &encoderClsid)) {
        json err;
        err["error"] = std::string("Image encoder not found for ") + mimeTypeStr;
        err["pid"]   = pid;
        return err.dump();
    }

    // Set up JPEG quality encoder parameter (ignored by PNG encoder).
    Gdiplus::EncoderParameters jpegParams;
    ULONG qualityValue = static_cast<ULONG>(quality);
    jpegParams.Count = 1;
    jpegParams.Parameter[0].Guid = Gdiplus::EncoderQuality;
    jpegParams.Parameter[0].Type = Gdiplus::EncoderParameterValueTypeLong;
    jpegParams.Parameter[0].NumberOfValues = 1;
    jpegParams.Parameter[0].Value = &qualityValue;

    Gdiplus::EncoderParameters* paramsPtr = useJpeg ? &jpegParams : nullptr;

    // Enumerate all visible top-level windows for the target PID.
    EnumContext ctx;
    ctx.targetPid = static_cast<DWORD>(pid);
    EnumWindows(EnumWindowsCallback, reinterpret_cast<LPARAM>(&ctx));

    json result;
    result["pid"]         = pid;
    result["windowCount"] = ctx.windows.size();
    result["format"]      = useJpeg ? "jpeg" : "png";
    result["quality"]     = quality;
    result["grayscale"]   = grayscale;
    result["windows"]     = json::array();

    if (ctx.windows.empty()) {
        result["error"] = "No visible windows found for PID " + std::to_string(pid);
        return result.dump();
    }

    for (auto& win : ctx.windows) {
        json wj;
        wj["hwnd"]         = reinterpret_cast<uintptr_t>(win.hwnd);
        wj["title"]        = WideToUtf8(win.title);
        wj["className"]    = WideToUtf8(win.className);
        wj["x"]            = static_cast<int>(win.rect.left);
        wj["y"]            = static_cast<int>(win.rect.top);
        wj["width"]        = static_cast<int>(win.rect.right  - win.rect.left);
        wj["height"]       = static_cast<int>(win.rect.bottom - win.rect.top);
        wj["isMainWindow"] = win.isMainWindow;
        wj["isModal"]      = win.isModal;
        wj["isEnabled"]    = win.isEnabled;
        wj["isMinimized"]  = win.isMinimized;
        wj["isMaximized"]  = win.isMaximized;
        wj["zOrder"]       = win.zOrder;

        // Always include ownerHwnd (0 means no owner / top-level window).
        wj["ownerHwnd"] = reinterpret_cast<uintptr_t>(win.ownerHwnd);

        // ownerIndex: index of the owner window in the windows array, -1 if none.
        int ownerIdx = -1;
        if (win.ownerHwnd) {
            for (int k = 0; k < static_cast<int>(ctx.windows.size()); k++) {
                if (ctx.windows[k].hwnd == win.ownerHwnd) { ownerIdx = k; break; }
            }
        }
        wj["ownerIndex"] = ownerIdx;

        // level: depth in the owner chain within captured windows (0 = root).
        int winLevel = 0;
        {
            HWND cur = win.ownerHwnd;
            while (cur != nullptr && winLevel < 32) {
                bool found = false;
                for (auto& w2 : ctx.windows) {
                    if (w2.hwnd == cur) { cur = w2.ownerHwnd; winLevel++; found = true; break; }
                }
                if (!found) break;
            }
        }
        wj["level"] = winLevel;

        // Capture the window contents.
        auto imgData = CaptureWindowToImage(win.hwnd, encoderClsid, paramsPtr, grayscale);

        if (!imgData.empty()) {
            wj["image"]     = Base64Encode(imgData.data(), imgData.size());
            wj["mimeType"]  = mimeTypeStr;
            wj["imageSize"] = imgData.size();
        } else {
            wj["error"] = "Failed to capture window";
        }

        result["windows"].push_back(wj);
    }

    return result.dump();
}

#endif // _WINDOWS
