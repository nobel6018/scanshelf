//! macOS 이미지 입출력과 테두리 감지. image 크레이트는 HEIC를 못 읽어 ImageIO를 쓴다.
//!
//! @see docs/pipeline.md

use std::path::Path;

use image::RgbImage;
use scanshelf_core::{Point, Quad};
use objc2::AnyThread;
use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext, CGDataProvider, CGImage,
    CGImageAlphaInfo, CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSArray, NSDictionary};
use objc2_image_io::{
    CGImageSource, kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceShouldCacheImmediately, kCGImageSourceThumbnailMaxPixelSize,
};
use objc2_vision::{
    VNDetectDocumentSegmentationRequest, VNDetectRectanglesRequest, VNImageRequestHandler, VNRectangleObservation, VNRequest,
};

use super::DecodeError;

/// EXIF 회전을 적용해 읽는다. 썸네일 API에 원본 크기 제한 없이 회전 적용을 맡기는 방식이 회전 처리를 직접 짜는 것보다 정확하다
pub fn decode_oriented(path: &Path, max_pixel_size: Option<u32>) -> Result<RgbImage, DecodeError> {
    let image = load_cgimage(path, max_pixel_size)?;
    Ok(cgimage_to_rgb(&image))
}

pub(crate) fn load_cgimage(path: &Path, max_pixel_size: Option<u32>) -> Result<CFRetained<CGImage>, DecodeError> {
    let url = CFURL::from_file_path(path).ok_or_else(|| DecodeError::Open(path.to_path_buf()))?;
    let source = unsafe { CGImageSource::with_url(&url, None) }.ok_or_else(|| DecodeError::Open(path.to_path_buf()))?;
    let yes: &CFType = CFBoolean::new(true);
    let mut keys: Vec<&CFString> = unsafe {
        vec![
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceShouldCacheImmediately,
        ]
    };
    let mut values: Vec<&CFType> = vec![yes, yes, yes];
    let max = max_pixel_size.map(|m| CFNumber::new_i32(m as i32));
    if let Some(max) = &max {
        keys.push(unsafe { kCGImageSourceThumbnailMaxPixelSize });
        values.push(max);
    }
    let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    unsafe { source.thumbnail_at_index(0, Some(options.as_ref())) }.ok_or_else(|| DecodeError::Decode(path.to_path_buf()))
}

/// sRGB RGBA 비트맵에 그려 RGB만 꺼낸다. 원본이 Display P3나 16비트여도 같은 형식이 된다
pub(crate) fn cgimage_to_rgb(image: &CGImage) -> RgbImage {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    let mut rgba = vec![0u8; width * height * 4];
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB 색 공간은 항상 있다");
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            Some(&space),
            CGImageAlphaInfo::NoneSkipLast.0,
        )
    }
    .expect("RGBA 비트맵 문맥 생성");
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&context), rect, Some(image));
    drop(context);
    let mut rgb = Vec::with_capacity(width * height * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&pixel[..3]);
    }
    RgbImage::from_raw(width as u32, height as u32, rgb).expect("버퍼 크기가 맞다")
}

pub(crate) fn rgb_to_cgimage(image: &RgbImage) -> CFRetained<CGImage> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let data = CFData::from_bytes(image.as_raw());
    let provider = CGDataProvider::with_cf_data(Some(&data)).expect("데이터 공급자 생성");
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB 색 공간은 항상 있다");
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            24,
            width * 3,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::None.0),
            Some(&provider),
            std::ptr::null(),
            true,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .expect("CGImage 생성")
}

/// Vision 문서 영역 감지. 손으로 들고 찍은 팜플렛처럼 배경과 대비가 약해도 사각형 감지보다 잘 잡는다
pub fn detect_document_quad(image: &RgbImage) -> Option<Quad> {
    autoreleasepool(|_| {
        let cgimage = rgb_to_cgimage(image);
        let options = NSDictionary::<objc2_foundation::NSString, AnyObject>::new();
        let handler = unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &cgimage, &options) };
        let request = unsafe { VNDetectDocumentSegmentationRequest::new() };
        let requests = NSArray::from_slice(&[request.as_ref() as &VNRequest]);
        handler.performRequests_error(&requests).ok()?;
        let results = unsafe { request.results() }?;
        let best = results.iter().max_by(|a, b| unsafe { a.confidence().total_cmp(&b.confidence()) })?;
        Some(to_quad(&best))
    })
}

/// Vision 사각형 감지. 밝은 책상 위 흰 종이처럼 문서 영역 감지가 놓치는 장을 테두리 직선으로 잡는다.
/// 펼친 책은 한 쪽씩도 사각형이라 후보를 여러 개 돌려주고 고르기는 부르는 쪽(pipeline::pick_rectangle)이 한다
pub fn detect_rectangle_quads(image: &RgbImage) -> Vec<Quad> {
    autoreleasepool(|_| {
        let cgimage = rgb_to_cgimage(image);
        let options = NSDictionary::<objc2_foundation::NSString, AnyObject>::new();
        let handler = unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &cgimage, &options) };
        let request = unsafe { VNDetectRectanglesRequest::new() };
        unsafe {
            // 펼침(가로 1.4)과 3단 접지(가로 2.1)까지 받는다. Vision 비율은 짧은 변/긴 변
            request.setMinimumAspectRatio(0.3);
            request.setMaximumAspectRatio(1.0);
            request.setMinimumSize(0.2);
            request.setQuadratureTolerance(30.0);
            request.setMinimumConfidence(0.5);
            request.setMaximumObservations(8);
        }
        let requests = NSArray::from_slice(&[request.as_ref() as &VNRequest]);
        if handler.performRequests_error(&requests).is_err() {
            return Vec::new();
        }
        unsafe { request.results() }.map(|r| r.iter().map(|o| to_quad(&o)).collect()).unwrap_or_default()
    })
}

/// Vision 좌표는 원점이 왼쪽 아래다. scanshelf 좌표(왼쪽 위)로 뒤집는다
fn to_quad(observation: &VNRectangleObservation) -> Quad {
    let flip = |p: CGPoint| Point {
        x: p.x.clamp(0.0, 1.0),
        y: (1.0 - p.y).clamp(0.0, 1.0),
    };
    unsafe {
        Quad {
            top_left: flip(observation.topLeft()),
            top_right: flip(observation.topRight()),
            bottom_right: flip(observation.bottomRight()),
            bottom_left: flip(observation.bottomLeft()),
        }
    }
}
